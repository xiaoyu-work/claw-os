// Tool helper for Claw OS Go apps.
//
// Apps that want to fulfil a model-proposed tool call (returned in
// AiResponse.ToolCalls after Chat(..., ChatOptions{Tools: ...})) shell
// out through this file to `cos ai tool <name> --app <id> --args
// <json>`. The kernel resolves the name against the catalog, derives
// the caps verb + scope, runs caps::require under the app's own grants,
// executes the implementation, and writes an audit row.

package clawossdk

import (
	"encoding/json"
	"os"
	"strings"
)

// ToolUnavailableError: the `cos` binary could not be invoked or
// returned garbage (transport failure).
type ToolUnavailableError struct{ Msg string }

func (e *ToolUnavailableError) Error() string { return e.Msg }

// ToolDeniedError: a gate (capability / unknown tool / args shape)
// refused the call. Payload holds the structured kernel envelope.
type ToolDeniedError struct{ Payload map[string]any }

func (e *ToolDeniedError) Error() string {
	if m, ok := e.Payload["error"].(string); ok && m != "" {
		return m
	}
	return "Tool call denied"
}

// ToolResult is the kernel-mediated result of one tool invocation.
// Value is the JSON the catalog implementation produced (per-tool shape).
type ToolResult struct {
	Name   string
	AppID  string
	Status string
	Value  any
	Raw    map[string]any
}

// CatalogEntry is one row from `cos ai tools`.
type CatalogEntry struct {
	Name          string
	Summary       string
	Verb          string
	Stability     string
	ArgsSchema    map[string]any
	ReturnsSchema map[string]any
	Raw           map[string]any
}

// CallTool invokes a catalog tool through the kernel. args may be nil. appID
// defaults to $COS_APP_ID. Returns a *ToolDeniedError for anything the
// gate refused, or *ToolUnavailableError for transport problems.
func CallTool(name string, args map[string]any, appID string) (*ToolResult, error) {
	if strings.TrimSpace(name) == "" {
		return nil, &ToolUnavailableError{Msg: "CallTool: name must be non-empty"}
	}
	app := appID
	if app == "" {
		app = os.Getenv("COS_APP_ID")
	}
	if app == "" {
		return nil, &ToolUnavailableError{Msg: name + ": app_id is required (pass appID or set COS_APP_ID)"}
	}
	if args == nil {
		args = map[string]any{}
	}
	payload, err := json.Marshal(args)
	if err != nil {
		return nil, &ToolUnavailableError{Msg: "Tool: could not encode args: " + err.Error()}
	}

	out, err := cosCallJSON("cos ai tool "+name, []string{"ai", "tool", name, "--app", app, "--args", string(payload)})
	if err != nil {
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &ToolUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if out.Status != 0 || out.hasError() {
		return nil, &ToolDeniedError{Payload: out.Envelope}
	}

	env := out.Envelope
	resName := name
	if s := asString(env["tool"]); s != "" {
		resName = s
	}
	resApp := app
	if s := asString(env["app_id"]); s != "" {
		resApp = s
	}
	status := "ok"
	if s := asString(env["status"]); s != "" {
		status = s
	}
	return &ToolResult{
		Name:   resName,
		AppID:  resApp,
		Status: status,
		Value:  env["result"],
		Raw:    env,
	}, nil
}

// Catalog returns the live tool catalog as exposed by `cos ai tools`.
// Apps shouldn't hard-code tool names without consulting this list; the
// catalog evolves and a tool can be deprecated or renamed.
func Catalog() ([]CatalogEntry, error) {
	out, err := cosCallJSON("cos ai tools", []string{"ai", "tools"})
	if err != nil {
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &ToolUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if out.Status != 0 || out.hasError() {
		return nil, &ToolDeniedError{Payload: out.Envelope}
	}
	rawRows, ok := out.Envelope["tools"].([]any)
	if !ok {
		return nil, &ToolUnavailableError{Msg: "cos ai tools envelope missing `tools` array"}
	}
	entries := make([]CatalogEntry, 0, len(rawRows))
	for _, r := range rawRows {
		row, ok := r.(map[string]any)
		if !ok {
			continue
		}
		stability := asString(row["stability"])
		if stability == "" {
			stability = "experimental"
		}
		entries = append(entries, CatalogEntry{
			Name:          asString(row["name"]),
			Summary:       asString(row["summary"]),
			Verb:          asString(row["verb"]),
			Stability:     stability,
			ArgsSchema:    maybeSchema(row["args_schema"]),
			ReturnsSchema: maybeSchema(row["returns_schema"]),
			Raw:           row,
		})
	}
	return entries, nil
}

// ForChat normalises tool names for ChatOptions.Tools: trims whitespace
// and drops empties, so ForChat("fs.read_text", " kv.get ", "") yields
// two clean entries.
func ForChat(names ...string) []string {
	out := make([]string, 0, len(names))
	for _, n := range names {
		s := strings.TrimSpace(n)
		if s != "" {
			out = append(out, s)
		}
	}
	return out
}

func maybeSchema(blob any) map[string]any {
	switch v := blob.(type) {
	case map[string]any:
		return v
	case string:
		var parsed map[string]any
		if json.Unmarshal([]byte(v), &parsed) == nil {
			return parsed
		}
	}
	return nil
}
