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

// CallTool invokes a catalog tool through the kernel. args may contain any
// JSON value; nil is encoded as explicit JSON null. appID defaults to
// $COS_APP_ID. Returns a *ToolDeniedError for anything the
// gate refused, or *ToolUnavailableError for transport problems.
func CallTool(name string, args any, appID string) (*ToolResult, error) {
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
	payload, err := json.Marshal(args)
	if err != nil {
		return nil, &ToolUnavailableError{Msg: "Tool: could not encode args: " + err.Error()}
	}

	out, err := cosCallJSON("cos ai tool "+name, []string{"ai", "tool", name, "--app", app, "--args", string(payload)})
	if err != nil {
		if denied, ok := err.(*DeniedError); ok {
			return nil, &ToolDeniedError{Payload: denied.Payload}
		}
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &ToolUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if err := ValidateTool(out.Data); err != nil {
		return nil, &ToolUnavailableError{Msg: "tool result decode failed: " + err.Error()}
	}
	env := asMap(out.Data)
	return &ToolResult{
		Name:   asString(env["tool"]),
		AppID:  asString(env["app_id"]),
		Status: asString(env["status"]),
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
		if denied, ok := err.(*DeniedError); ok {
			return nil, &ToolDeniedError{Payload: denied.Payload}
		}
		if ue, ok := err.(*UnavailableError); ok {
			return nil, &ToolUnavailableError{Msg: ue.Msg}
		}
		return nil, err
	}
	if err := ValidateToolCatalog(out.Data); err != nil {
		return nil, &ToolUnavailableError{Msg: "catalog decode failed: " + err.Error()}
	}
	env := asMap(out.Data)
	rawRows := env["tools"].([]any)
	entries := make([]CatalogEntry, 0, len(rawRows))
	for _, r := range rawRows {
		row := r.(map[string]any)
		entries = append(entries, CatalogEntry{
			Name:          asString(row["name"]),
			Summary:       asString(row["summary"]),
			Verb:          asString(row["verb"]),
			Stability:     asString(row["stability"]),
			ArgsSchema:    asMap(row["args_schema"]),
			ReturnsSchema: asMap(row["returns_schema"]),
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
