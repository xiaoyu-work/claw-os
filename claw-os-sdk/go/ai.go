// AI helper for Claw OS Go apps.
//
// Chat is the stable model API and shells out to
// `cos ai chat --app <id>`. The kernel derives ai.chat or
// ai.chat.untrusted from origin, then runs capability checks, budget,
// safety, and audit.
//
// Apps never name a verb and never pick a model. They describe what
// they want; the gate picks the verb and the machine owner configures
// the one provider/model in /etc/cos/agent.toml. Importing a provider
// SDK directly is refused at install time by `cos app lint` — this is
// the only sanctioned route to a model.
//
//	res, err := clawossdk.Chat("Summarise this", clawossdk.ChatOptions{
//		Origin: "external-content", AppID: "notes",
//	})

package clawossdk

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

// AiUnavailableError: the `cos` binary could not be invoked or returned
// garbage (transport failure). Distinct from a gate refusal.
type AiUnavailableError struct{ Msg string }

func (e *AiUnavailableError) Error() string { return e.Msg }

// AiDeniedError: a gate (capability / origin) refused the call.
type AiDeniedError struct{ Payload map[string]any }

func (e *AiDeniedError) Error() string {
	if m, ok := e.Payload["error"].(string); ok && m != "" {
		return m
	}
	return "AI call denied"
}

// AiBudgetExceededError: the per-app monthly budget was exhausted.
type AiBudgetExceededError struct{ Payload map[string]any }

func (e *AiBudgetExceededError) Error() string {
	if m, ok := e.Payload["error"].(string); ok && m != "" {
		return m
	}
	return "AI budget exceeded"
}

// AiSafetyViolationError: the safety pipeline refused the request.
type AiSafetyViolationError struct{ Payload map[string]any }

func (e *AiSafetyViolationError) Error() string {
	if m, ok := e.Payload["error"].(string); ok && m != "" {
		return m
	}
	return "AI safety violation"
}

// ProposedToolCall is a tool call the model proposed but the gate did
// NOT execute. Apps decide whether to fulfil any by calling Tool with
// the same Name + Input; ID echoes back to the provider next turn.
type ProposedToolCall struct {
	ID    string `json:"id"`
	Name  string `json:"name"`
	Input any    `json:"input"`
}

// AiResponse is the parsed reply from the AI gate. It reuses the
// generated AiUsage / AiBudget / AiReview wire types.
type AiResponse struct {
	Text      string
	Model     string
	Provider  string
	Verb      string
	Usage     AiUsage
	Budget    AiBudget
	Review    AiReview
	ToolCalls []ProposedToolCall
	Raw       map[string]any
}

// ChatOptions are the optional parameters for Chat.
type ChatOptions struct {
	// Origin is the prompt provenance. Pass "external-content" for any
	// third-party text so the strict safety pipeline runs and the gate
	// picks ai.chat.untrusted. Defaults to "trusted".
	Origin string
	// MaxUnits caps the spend for this call (0 = let the gate decide).
	MaxUnits int
	// System is an optional system prompt for chat.
	System string
	// AppID is the app identity; defaults to $COS_APP_ID.
	AppID string
	// Tools lists catalog tool names the model may *propose*; the gate
	// never executes them — proposals return in AiResponse.ToolCalls.
	Tools []string
}

type dispatchArgs struct {
	modality string
	prompt   *string
	opts     ChatOptions
}

func strptr(s string) *string { return &s }

// Chat sends a single-shot chat completion through the kernel's AI gate.
func Chat(prompt string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "Chat: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "chat", prompt: strptr(prompt), opts: opts})
}

// Budget returns the current-period budget snapshot for an app. appID
// defaults to $COS_APP_ID when empty.
func Budget(appID string) (*AiBudget, error) {
	app := appID
	if app == "" {
		app = os.Getenv("COS_APP_ID")
	}
	if app == "" {
		return nil, &AiUnavailableError{Msg: "Budget: app_id is required"}
	}
	out, err := cosCallJSON("cos agent budget show", []string{"agent", "budget", "show", app})
	if err != nil {
		if denied, ok := err.(*DeniedError); ok {
			return nil, &AiUnavailableError{Msg: denied.Error()}
		}
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &AiUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if err := ValidateBudgetShow(out.Data); err != nil {
		return nil, &AiUnavailableError{Msg: "budget response decode failed: " + err.Error()}
	}
	env := asMap(out.Data)
	return &AiBudget{
		Period:    asString(env["period"]),
		UnitsUsed: asUint64(env["units_used"]),
		UnitsCap:  0,
	}, nil
}

func resolveApp(modality, appID string) (string, error) {
	app := appID
	if app == "" {
		app = os.Getenv("COS_APP_ID")
	}
	if app == "" {
		return "", &AiUnavailableError{Msg: modality + ": app_id is required (set AppID or COS_APP_ID)"}
	}
	return app, nil
}

func writePrivateInput(name, value string) (string, func(), error) {
	file, err := os.CreateTemp("", "claw-ai-"+name+"-*")
	if err != nil {
		return "", nil, err
	}
	path := file.Name()
	cleanup := func() { _ = os.Remove(path) }
	if err := file.Chmod(0o600); err != nil {
		_ = file.Close()
		cleanup()
		return "", nil, err
	}
	if _, err := file.WriteString(value); err != nil {
		_ = file.Close()
		cleanup()
		return "", nil, err
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		cleanup()
		return "", nil, err
	}
	if err := file.Close(); err != nil {
		cleanup()
		return "", nil, err
	}
	return path, cleanup, nil
}

func dispatch(a dispatchArgs) (*AiResponse, error) {
	origin := a.opts.Origin
	if origin == "" {
		origin = "trusted"
	}
	app, err := resolveApp(a.modality, a.opts.AppID)
	if err != nil {
		return nil, err
	}

	argv := []string{"ai", "chat", "--app", app, "--origin", origin}
	cleanups := make([]func(), 0, 2)
	defer func() {
		for i := len(cleanups) - 1; i >= 0; i-- {
			cleanups[i]()
		}
	}()
	if a.prompt != nil {
		path, cleanup, err := writePrivateInput("prompt", *a.prompt)
		if err != nil {
			return nil, &AiUnavailableError{Msg: fmt.Sprintf("write private prompt: %v", err)}
		}
		cleanups = append(cleanups, cleanup)
		argv = append(argv, "--prompt-file", path)
	}
	if a.opts.MaxUnits != 0 {
		argv = append(argv, "--max-units", strconv.Itoa(a.opts.MaxUnits))
	}
	if a.opts.System != "" {
		path, cleanup, err := writePrivateInput("system", a.opts.System)
		if err != nil {
			return nil, &AiUnavailableError{Msg: fmt.Sprintf("write private system prompt: %v", err)}
		}
		cleanups = append(cleanups, cleanup)
		argv = append(argv, "--system-file", path)
	}
	if len(a.opts.Tools) > 0 {
		argv = append(argv, "--tools", strings.Join(a.opts.Tools, ","))
	}

	out, err := cosCallJSON("cos ai "+a.modality, argv)
	if err != nil {
		if denied, ok := err.(*DeniedError); ok {
			return nil, classifyError(denied.Payload)
		}
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &AiUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	response, err := parseResponse(out.Data)
	if err != nil {
		return nil, err
	}
	return response, nil
}

func classifyError(env map[string]any) error {
	code := strings.ToUpper(asString(env["code"]))
	switch code {
	case "BUDGET_EXCEEDED":
		return &AiBudgetExceededError{Payload: env}
	case "SAFETY_VIOLATION":
		return &AiSafetyViolationError{Payload: env}
	}
	return &AiDeniedError{Payload: env}
}

func parseBudget(blk map[string]any) AiBudget {
	return AiBudget{
		Period:    asString(blk["period"]),
		UnitsUsed: asUint64(blk["units_used"]),
		UnitsCap:  asUint64(blk["units_cap"]),
	}
}

func parseResponse(value any) (*AiResponse, error) {
	if err := ValidateAi(value); err != nil {
		return nil, &AiUnavailableError{Msg: "ai response decode failed: " + err.Error()}
	}
	env := asMap(value)
	usage := asMap(env["usage"])
	review := asMap(env["review"])

	var calls []ProposedToolCall
	if raw, present := env["tool_calls"]; present {
		rawCalls := raw.([]any)
		calls = make([]ProposedToolCall, 0, len(rawCalls))
		for _, c := range rawCalls {
			cm := c.(map[string]any)
			calls = append(calls, ProposedToolCall{
				ID:    asString(cm["id"]),
				Name:  asString(cm["name"]),
				Input: cm["input"],
			})
		}
	}

	return &AiResponse{
		Text:     asString(env["text"]),
		Model:    asString(env["model"]),
		Provider: asString(env["provider"]),
		Verb:     asString(env["verb"]),
		Usage: AiUsage{
			InputTokens:  asUint32(usage["input_tokens"]),
			OutputTokens: asUint32(usage["output_tokens"]),
			Units:        asUint64(usage["units"]),
		},
		Budget: parseBudget(asMap(env["budget"])),
		Review: AiReview{
			Safety:         asString(review["safety"]),
			PromptRedacted: asBool(review["prompt_redacted"]),
		},
		ToolCalls: calls,
		Raw:       env,
	}, nil
}
