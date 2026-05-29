// AI helper for Claw OS Go apps.
//
// Every Go app that needs a model (LLM, embedding, image, TTS, STT,
// vision, video) goes through this file. It shells out to
// `cos ai chat --app <id>` — the single authoritative entry point for
// AI requests of every modality. The kernel derives the modality (and
// the underlying caps verb) from the request shape, then runs the
// capability check, prompt-origin allowlist, per-month budget, safety
// pipeline, and audit before any model sees the prompt.
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
	ID    string         `json:"id"`
	Name  string         `json:"name"`
	Input map[string]any `json:"input"`
}

// AiResponse is the parsed reply from the AI gate. It reuses the
// generated AiUsage / AiBudget / AiReview wire types.
type AiResponse struct {
	Text       string
	Model      string
	Provider   string
	Verb       string
	Embedding  []float64
	OutputPath string
	Usage      AiUsage
	Budget     AiBudget
	Review     AiReview
	ToolCalls  []ProposedToolCall
	Raw        map[string]any
}

// ChatOptions are the optional parameters shared by the AI helpers.
type ChatOptions struct {
	// Origin is the prompt provenance. Pass "external-content" for any
	// third-party text so the strict safety pipeline runs and the gate
	// picks ai.chat.untrusted. Defaults to "trusted".
	Origin string
	// MaxUnits caps the spend for this call (0 = let the gate decide).
	MaxUnits int
	// System is an optional system prompt (chat / vision).
	System string
	// AppID is the app identity; defaults to $COS_APP_ID.
	AppID string
	// Tools lists catalog tool names the model may *propose*; the gate
	// never executes them — proposals return in AiResponse.ToolCalls.
	Tools []string
}

type dispatchArgs struct {
	modality    string
	prompt      *string
	opts        ChatOptions
	embed       bool
	imageInput  string
	imageOutput string
	audioInput  string
	audioOutput string
	videoInput  string
	videoOutput string
}

func strptr(s string) *string { return &s }

// Chat sends a single-shot chat completion through the kernel's AI gate.
func Chat(prompt string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "Chat: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "chat", prompt: strptr(prompt), opts: opts})
}

// Embed embeds text into a vector. The vector is at AiResponse.Embedding.
func Embed(prompt string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "Embed: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "embed", prompt: strptr(prompt), opts: opts, embed: true})
}

// ImageGenerate generates an image from a prompt; the gate writes it to output.
func ImageGenerate(prompt, output string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "ImageGenerate: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "image.generate", prompt: strptr(prompt), opts: opts, imageOutput: output})
}

// ImageAnalyze captions / classifies an image with no prompt.
func ImageAnalyze(image string, opts ChatOptions) (*AiResponse, error) {
	return dispatch(dispatchArgs{modality: "image.analyze", opts: opts, imageInput: image})
}

// VisionAnalyze answers a textual question about an image.
func VisionAnalyze(prompt, image string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "VisionAnalyze: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "vision.analyze", prompt: strptr(prompt), opts: opts, imageInput: image})
}

// AudioTTS synthesizes speech from text; the gate writes audio to output.
func AudioTTS(prompt, output string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "AudioTTS: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "audio.tts", prompt: strptr(prompt), opts: opts, audioOutput: output})
}

// AudioSTT transcribes speech to text from an audio file.
func AudioSTT(audio string, opts ChatOptions) (*AiResponse, error) {
	return dispatch(dispatchArgs{modality: "audio.stt", opts: opts, audioInput: audio})
}

// VideoGenerate generates a video from a prompt; the gate writes it to output.
func VideoGenerate(prompt, output string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "VideoGenerate: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "video.generate", prompt: strptr(prompt), opts: opts, videoOutput: output})
}

// VideoAnalyze answers a textual question about a video.
func VideoAnalyze(prompt, video string, opts ChatOptions) (*AiResponse, error) {
	if strings.TrimSpace(prompt) == "" {
		return nil, &AiUnavailableError{Msg: "VideoAnalyze: prompt must be non-empty"}
	}
	return dispatch(dispatchArgs{modality: "video.analyze", prompt: strptr(prompt), opts: opts, videoInput: video})
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
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &AiUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if out.Status != 0 || out.hasError() {
		return nil, &AiUnavailableError{Msg: "cos agent budget show failed: " + asString(out.Envelope["error"])}
	}
	b := parseBudget(out.Envelope)
	return &b, nil
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
	if a.prompt != nil {
		argv = append(argv, "--prompt", *a.prompt)
	}
	if a.opts.MaxUnits != 0 {
		argv = append(argv, "--max-units", strconv.Itoa(a.opts.MaxUnits))
	}
	if a.opts.System != "" {
		argv = append(argv, "--system", a.opts.System)
	}
	if a.embed {
		argv = append(argv, "--embed")
	}
	if a.imageInput != "" {
		argv = append(argv, "--image-input", a.imageInput)
	}
	if a.imageOutput != "" {
		argv = append(argv, "--image-output", a.imageOutput)
	}
	if a.audioInput != "" {
		argv = append(argv, "--audio-input", a.audioInput)
	}
	if a.audioOutput != "" {
		argv = append(argv, "--audio-output", a.audioOutput)
	}
	if a.videoInput != "" {
		argv = append(argv, "--video-input", a.videoInput)
	}
	if a.videoOutput != "" {
		argv = append(argv, "--video-output", a.videoOutput)
	}
	if len(a.opts.Tools) > 0 {
		argv = append(argv, "--tools", strings.Join(a.opts.Tools, ","))
	}

	out, err := cosCallJSON("cos ai "+a.modality, argv)
	if err != nil {
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &AiUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if out.Status != 0 || out.hasError() {
		return nil, classifyError(out.Envelope)
	}
	return parseResponse(out.Envelope), nil
}

func classifyError(env map[string]any) error {
	msg := strings.ToLower(asString(env["error"]))
	if strings.Contains(msg, "budget") && (strings.Contains(msg, "exceed") || strings.Contains(msg, "over")) {
		return &AiBudgetExceededError{Payload: env}
	}
	if strings.Contains(msg, "safety") || strings.Contains(msg, "redact") || strings.Contains(msg, "injection") {
		return &AiSafetyViolationError{Payload: env}
	}
	return &AiDeniedError{Payload: env}
}

func parseBudget(blk map[string]any) AiBudget {
	return AiBudget{
		Period:    asString(blk["period"]),
		UnitsUsed: asFloat(blk["units_used"]),
		UnitsCap:  asFloat(blk["units_cap"]),
	}
}

func parseResponse(env map[string]any) *AiResponse {
	usage := asMap(env["usage"])
	review := asMap(env["review"])

	var embedding []float64
	if raw, ok := env["embedding"].([]any); ok {
		for _, x := range raw {
			embedding = append(embedding, asFloat(x))
		}
	}

	var calls []ProposedToolCall
	if raw, ok := env["tool_calls"].([]any); ok {
		for _, c := range raw {
			cm, ok := c.(map[string]any)
			if !ok {
				continue
			}
			calls = append(calls, ProposedToolCall{
				ID:    asString(cm["id"]),
				Name:  asString(cm["name"]),
				Input: asMap(cm["input"]),
			})
		}
	}

	return &AiResponse{
		Text:       asString(env["text"]),
		Model:      asString(env["model"]),
		Provider:   asString(env["provider"]),
		Verb:       asString(env["verb"]),
		Embedding:  embedding,
		OutputPath: asString(env["output_path"]),
		Usage: AiUsage{
			InputTokens:  asInt(usage["input_tokens"]),
			OutputTokens: asInt(usage["output_tokens"]),
			Units:        asFloat(usage["units"]),
		},
		Budget: parseBudget(asMap(env["budget"])),
		Review: AiReview{
			Safety: func() string {
				s := asString(review["safety"])
				if s == "" {
					return "strict"
				}
				return s
			}(),
			PromptRedacted: asBool(review["prompt_redacted"]),
		},
		ToolCalls: calls,
		Raw:       env,
	}
}
