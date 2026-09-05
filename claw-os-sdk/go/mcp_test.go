package clawossdk

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestRemovedCrossAppContractIsRejected(t *testing.T) {
	path := writeMCPRawManifest(t, `{"schema_version":2,"id":"example","version":"1","name":{"en":"Example"},"mcp":{"access":{"apps":[]},"tools":[{"name":"example.noop","summary":{"en":"Noop"}}]}}`)
	if _, err := LoadMCPApp(path); err == nil {
		t.Fatal("removed App access allowlist accepted")
	}
	for _, field := range []string{"app", "app-agent", "app_id", "depth", "parent_call_id"} {
		t.Run(field, func(t *testing.T) {
			app, err := LoadMCPApp(writeMCPTestManifest(t, `{"name":"example.noop","summary":{"en":"Noop"}}`))
			if err != nil {
				t.Fatal(err)
			}
			if err := app.Bind("example.noop", func(map[string]any, *MCPCall) (any, error) {
				t.Error("rejected call reached handler")
				return nil, nil
			}); err != nil {
				t.Fatal(err)
			}
			value := decodeWireValue(t, validMCPContextJSON("removed", 0)).(map[string]any)
			code := WireUnknownField
			switch field {
			case "app", "app-agent":
				value["caller"].(map[string]any)["kind"] = field
				code = WireEnum
			case "app_id":
				value["caller"].(map[string]any)[field] = nil
			default:
				value[field] = nil
			}
			err = ValidateMcpCallContext(value)
			if wire, ok := err.(*WireDecodeError); !ok || wire.Code != code {
				t.Fatalf("wire error = %v", err)
			}
			meta, err := json.Marshal(map[string]any{MCPCallContextMetaKey: value})
			if err != nil {
				t.Fatal(err)
			}
			frames := serveMCPFrames(t, app, mcpCallFrame("1", "example.noop", "{}", string(meta)))
			fault := frames[0]["error"].(map[string]any)
			if fault["code"].(json.Number).String() != "-32602" {
				t.Fatalf("fault = %#v", fault)
			}
		})
	}
}

func writeMCPTestManifest(t *testing.T, tools string) string {
	t.Helper()
	return writeMCPRawManifest(t, fmt.Sprintf(`{
		"schema_version": 2,
		"id": "example",
		"version": "1.2.3",
		"name": {"en": "Example"},
		"mcp": {"transport": "stdio", "tools": [%s]}
	}`, tools))
}

func writeMCPRawManifest(t *testing.T, body string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "app.json")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func validMCPContextJSON(callID string, deadline int64) string {
	deadlineField := ""
	if deadline != 0 {
		deadlineField = fmt.Sprintf(`,"deadline_unix_ms":%d`, deadline)
	}
	return fmt.Sprintf(
		`{"wire_version":1.0,"call_id":%q,"trace_id":"trace-1","session_id":"session-1","task_id":"task-1","caller":{"kind":"system-agent","id":"caller-agent","owner_uid":1e3}%s}`,
		callID,
		deadlineField,
	)
}

func mcpCallFrame(id, name, arguments, meta string) string {
	if arguments == "" {
		arguments = "{}"
	}
	if meta == "" {
		callID := strings.ReplaceAll(strings.Trim(id, `"`), "+", "p")
		meta = fmt.Sprintf(`{"%s":%s}`, MCPCallContextMetaKey, validMCPContextJSON("call-"+callID, 0))
	}
	return fmt.Sprintf(
		`{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":%q,"arguments":%s,"_meta":%s}}`,
		id, name, arguments, meta,
	)
}

func serveMCPFrames(t *testing.T, app *MCPApp, frames ...string) []map[string]any {
	t.Helper()
	var output bytes.Buffer
	input := strings.NewReader(strings.Join(frames, "\n") + "\n")
	if err := app.Serve(input, &output); err != nil {
		t.Fatal(err)
	}
	return decodeMCPFrames(t, output.Bytes())
}

func decodeMCPFrames(t *testing.T, raw []byte) []map[string]any {
	t.Helper()
	lines := bytes.Split(bytes.TrimSpace(raw), []byte{'\n'})
	if len(lines) == 1 && len(lines[0]) == 0 {
		return nil
	}
	frames := make([]map[string]any, 0, len(lines))
	for _, line := range lines {
		var frame map[string]any
		if err := decodeMCPJSON(line, &frame); err != nil {
			t.Fatalf("decode output %q: %v", line, err)
		}
		frames = append(frames, frame)
	}
	return frames
}

func frameByID(t *testing.T, frames []map[string]any, id string) map[string]any {
	t.Helper()
	for _, frame := range frames {
		raw, err := json.Marshal(frame["id"])
		if err == nil && string(raw) == id {
			return frame
		}
	}
	t.Fatalf("no frame with id %s in %#v", id, frames)
	return nil
}

func TestMCPManifestListBindingContextArgumentsAndResults(t *testing.T) {
	path := writeMCPTestManifest(t, `{
		"name":"example.run",
		"summary":{"en":"Run the example"},
		"args":[
			{"name":"mode","kind":"text","required":true,"choices":["fast","slow"],"label":{"en":"Run mode"}},
			{"name":"count","kind":"integer","default":900719925474099312345},
			{"name":"detail","kind":"text","required_when":{"kind":"arg-equals","arg":"mode","value":"slow"}}
		]
	}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if app.ID() != "example" || app.Version() != "1.2.3" {
		t.Fatalf("identity = %s@%s", app.ID(), app.Version())
	}

	var gotContext McpCallContext
	var gotCount json.Number
	if err := app.Bind("example.run", func(args map[string]any, call *MCPCall) (any, error) {
		gotContext = call.Authenticated()
		gotCount = args["count"].(json.Number)
		return map[string]any{"mode": args["mode"], "count": args["count"]}, nil
	}); err != nil {
		t.Fatal(err)
	}
	if err := app.Bind("not.declared", func(map[string]any, *MCPCall) (any, error) { return nil, nil }); err == nil {
		t.Fatal("binding an undeclared tool succeeded")
	}

	largeID := "900719925474099312345"
	frames := serveMCPFrames(t, app,
		`{"jsonrpc":"2.0","id":"list","method":"tools/list"}`,
		mcpCallFrame(largeID, "example.run", `{"mode":"fast"}`, ""),
		mcpCallFrame(`"bad"`, "example.run", `{"mode":"fast","extra":true}`, ""),
		mcpCallFrame(`"missing-context"`, "example.run", `{}`, `{}`),
	)

	list := frameByID(t, frames, `"list"`)["result"].(map[string]any)
	tool := list["tools"].([]any)[0].(map[string]any)
	if tool["name"] != "example.run" || tool["description"] != "Run the example" {
		t.Fatalf("tool = %#v", tool)
	}
	schema := tool["inputSchema"].(map[string]any)
	if schema["additionalProperties"] != false {
		t.Fatalf("input schema = %#v", schema)
	}
	properties := schema["properties"].(map[string]any)
	if properties["count"].(map[string]any)["default"].(json.Number).String() != largeID {
		t.Fatalf("default lost numeric precision: %#v", properties["count"])
	}
	if len(schema["allOf"].([]any)) != 1 {
		t.Fatalf("conditional schema = %#v", schema["allOf"])
	}

	call := frameByID(t, frames, largeID)
	result := call["result"].(map[string]any)
	if result["isError"] != false {
		t.Fatalf("call result = %#v", result)
	}
	structured := result["structuredContent"].(map[string]any)
	if structured["count"].(json.Number).String() != largeID {
		t.Fatalf("structured number = %#v", structured["count"])
	}
	if gotCount.String() != largeID {
		t.Fatalf("handler count = %s", gotCount)
	}
	if gotContext.CallId != "call-"+largeID ||
		gotContext.Caller.Id != "caller-agent" ||
		gotContext.SessionId != "session-1" {
		t.Fatalf("authenticated context = %#v", gotContext)
	}

	bad := frameByID(t, frames, `"bad"`)["result"].(map[string]any)
	if bad["isError"] != true || !strings.Contains(bad["content"].([]any)[0].(map[string]any)["text"].(string), "unknown argument") {
		t.Fatalf("bad arguments result = %#v", bad)
	}
	missing := frameByID(t, frames, `"missing-context"`)
	if missing["error"].(map[string]any)["code"].(json.Number).String() != "-32602" {
		t.Fatalf("missing context response = %#v", missing)
	}
}

func TestMCPPreservesLargeNumericArgumentLexeme(t *testing.T) {
	path := writeMCPTestManifest(t, `{
		"name":"example.number",
		"summary":{"en":"Read a number"},
		"args":[{"name":"value","kind":"number","required":true}]
	}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	var lexeme string
	if err := app.Bind("example.number", func(args map[string]any, _ *MCPCall) (any, error) {
		lexeme = args["value"].(json.Number).String()
		return args["value"], nil
	}); err != nil {
		t.Fatal(err)
	}
	const number = "12345678901234567890.123400e+17"
	frames := serveMCPFrames(t, app, mcpCallFrame(number, "example.number", `{"value":`+number+`}`, ""))
	if lexeme != number {
		t.Fatalf("argument lexeme = %q", lexeme)
	}
	if !bytes.Contains(mustJSON(t, frames[0]), []byte(`"id":`+number)) {
		t.Fatalf("response ID lost precision: %#v", frames[0]["id"])
	}
	text := frames[0]["result"].(map[string]any)["content"].([]any)[0].(map[string]any)["text"]
	if text != number {
		t.Fatalf("result text = %q", text)
	}
}

func TestMCPProgressAndExplicitResults(t *testing.T) {
	path := writeMCPTestManifest(t, `{
		"name":"example.progress",
		"summary":{"en":"Report progress"}
	}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := app.Bind("example.progress", func(_ map[string]any, call *MCPCall) (any, error) {
		if !call.ProgressRequested() {
			return ErrorMCPResult("progress token missing"), nil
		}
		total := 10.0
		if err := call.ReportProgress(4, MCPProgress{Total: &total, Message: "working"}); err != nil {
			return nil, err
		}
		return StructuredMCPResult(map[string]any{"ok": true}, "done")
	}); err != nil {
		t.Fatal(err)
	}
	meta := fmt.Sprintf(`{"progressToken":"token-1","%s":%s}`, MCPCallContextMetaKey, validMCPContextJSON("progress-call", 0))
	frames := serveMCPFrames(t, app, mcpCallFrame(`"progress"`, "example.progress", `{}`, meta))
	if len(frames) != 2 {
		t.Fatalf("frames = %#v", frames)
	}
	if frames[0]["method"] != "notifications/progress" {
		t.Fatalf("progress frame = %#v", frames[0])
	}
	params := frames[0]["params"].(map[string]any)
	if params["progressToken"] != "token-1" || params["message"] != "working" {
		t.Fatalf("progress params = %#v", params)
	}
	result := frames[1]["result"].(map[string]any)
	if result["structuredContent"].(map[string]any)["ok"] != true ||
		result["content"].([]any)[0].(map[string]any)["text"] != "done" {
		t.Fatalf("structured result = %#v", result)
	}
}

func TestMCPHandlerPanicIsIsolatedToTheCall(t *testing.T) {
	path := writeMCPTestManifest(t, `{
		"name":"example.panic",
		"summary":{"en":"Test handler isolation"},
		"args":[{"name":"panic","kind":"bool","default":false}]
	}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := app.Bind("example.panic", func(args map[string]any, _ *MCPCall) (any, error) {
		if args["panic"].(bool) {
			panic("boom")
		}
		return "still serving", nil
	}); err != nil {
		t.Fatal(err)
	}
	frames := serveMCPFrames(t, app,
		mcpCallFrame(`"panic"`, "example.panic", `{"panic":true}`, ""),
		mcpCallFrame(`"after"`, "example.panic", `{}`, ""),
	)
	failed := frameByID(t, frames, `"panic"`)["result"].(map[string]any)
	if failed["isError"] != true ||
		!strings.Contains(failed["content"].([]any)[0].(map[string]any)["text"].(string), "handler panicked: boom") {
		t.Fatalf("panic result = %#v", failed)
	}
	succeeded := frameByID(t, frames, `"after"`)["result"].(map[string]any)
	if succeeded["isError"] != false ||
		succeeded["content"].([]any)[0].(map[string]any)["text"] != "still serving" {
		t.Fatalf("subsequent result = %#v", succeeded)
	}
}

func TestMCPCancellationSuppressesObsoleteResponse(t *testing.T) {
	path := writeMCPTestManifest(t, `{"name":"example.wait","summary":{"en":"Wait"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	started := make(chan struct{})
	if err := app.Bind("example.wait", func(_ map[string]any, call *MCPCall) (any, error) {
		close(started)
		<-call.Done()
		return nil, call.CheckCancelled()
	}); err != nil {
		t.Fatal(err)
	}

	inputReader, inputWriter := io.Pipe()
	outputReader, outputWriter := io.Pipe()
	done := make(chan error, 1)
	go func() { done <- app.Serve(inputReader, outputWriter) }()
	writePipeFrame(t, inputWriter, mcpCallFrame(`"wait"`, "example.wait", `{}`, ""))
	waitChannel(t, started, "handler start")
	writePipeFrame(t, inputWriter, `{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"wait"}}`)
	writePipeFrame(t, inputWriter, `{"jsonrpc":"2.0","id":"ping","method":"ping"}`)

	line, err := bufio.NewReader(outputReader).ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	frames := decodeMCPFrames(t, line)
	if len(frames) != 1 || frames[0]["id"] != "ping" {
		t.Fatalf("cancelled call emitted a response: %#v", frames)
	}
	_ = inputWriter.Close()
	if err := waitError(t, done); err != nil {
		t.Fatal(err)
	}
	_ = outputReader.Close()
}

func TestMCPDeadlineCancelsRunningHandler(t *testing.T) {
	path := writeMCPTestManifest(t, `{"name":"example.wait","summary":{"en":"Wait"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := app.Bind("example.wait", func(_ map[string]any, call *MCPCall) (any, error) {
		<-call.Done()
		return nil, call.CheckCancelled()
	}); err != nil {
		t.Fatal(err)
	}
	inputReader, inputWriter := io.Pipe()
	outputReader, outputWriter := io.Pipe()
	done := make(chan error, 1)
	go func() { done <- app.Serve(inputReader, outputWriter) }()
	deadline := time.Now().Add(40 * time.Millisecond).UnixMilli()
	meta := fmt.Sprintf(`{"%s":%s}`, MCPCallContextMetaKey, validMCPContextJSON("deadline-call", deadline))
	writePipeFrame(t, inputWriter, mcpCallFrame(`"deadline"`, "example.wait", `{}`, meta))
	line, err := bufio.NewReader(outputReader).ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	result := decodeMCPFrames(t, line)[0]["result"].(map[string]any)
	if result["isError"] != true ||
		!strings.Contains(result["content"].([]any)[0].(map[string]any)["text"].(string), "deadline") {
		t.Fatalf("deadline result = %#v", result)
	}
	_ = inputWriter.Close()
	if err := waitError(t, done); err != nil {
		t.Fatal(err)
	}
	_ = outputReader.Close()
}

func TestMCPMalformedAndOversizedFramesRecover(t *testing.T) {
	path := writeMCPTestManifest(t, `{"name":"example.noop","summary":{"en":"No operation"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := app.Bind("example.noop", func(map[string]any, *MCPCall) (any, error) {
		return nil, nil
	}); err != nil {
		t.Fatal(err)
	}
	var input bytes.Buffer
	input.WriteString("{\n")
	input.Write(bytes.Repeat([]byte{'x'}, MaxMCPFrameBytes+1))
	input.WriteByte('\n')
	input.WriteString(`{"jsonrpc":"2.0","id":"ping","method":"ping"}`)
	input.WriteByte('\n')
	var output bytes.Buffer
	if err := app.Serve(&input, &output); err != nil {
		t.Fatal(err)
	}
	frames := decodeMCPFrames(t, output.Bytes())
	if len(frames) != 3 {
		t.Fatalf("frames = %d", len(frames))
	}
	if frames[0]["error"].(map[string]any)["code"].(json.Number).String() != "-32700" ||
		frames[1]["error"].(map[string]any)["code"].(json.Number).String() != "-32700" ||
		frames[2]["id"] != "ping" {
		t.Fatalf("recovery frames = %#v", frames)
	}
}

type failingMCPWriter struct {
	err error
}

func (w failingMCPWriter) Write([]byte) (int, error) { return 0, w.err }

func TestMCPOutputFailureCancelsOutstandingCall(t *testing.T) {
	path := writeMCPTestManifest(t, `{"name":"example.wait","summary":{"en":"Wait"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	started := make(chan struct{})
	cancelled := make(chan struct{})
	if err := app.Bind("example.wait", func(_ map[string]any, call *MCPCall) (any, error) {
		close(started)
		<-call.Done()
		close(cancelled)
		return nil, call.CheckCancelled()
	}); err != nil {
		t.Fatal(err)
	}
	inputReader, inputWriter := io.Pipe()
	done := make(chan error, 1)
	go func() {
		done <- app.Serve(inputReader, failingMCPWriter{err: errors.New("write boom")})
	}()
	writePipeFrame(t, inputWriter, mcpCallFrame(`"wait"`, "example.wait", `{}`, ""))
	waitChannel(t, started, "handler start")
	writePipeFrame(t, inputWriter, `{"jsonrpc":"2.0","id":"ping","method":"ping"}`)
	waitChannel(t, cancelled, "handler cancellation")
	serveErr := waitError(t, done)
	if serveErr == nil || !strings.Contains(serveErr.Error(), "write boom") {
		t.Fatalf("Serve error = %v", serveErr)
	}
	_ = inputWriter.Close()
}

func TestMCPQueueBoundAndSingleActiveCall(t *testing.T) {
	path := writeMCPTestManifest(t, `{"name":"example.wait","summary":{"en":"Wait"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	var active atomic.Int32
	var maximum atomic.Int32
	var starts atomic.Int32
	if err := app.Bind("example.wait", func(_ map[string]any, call *MCPCall) (any, error) {
		starts.Add(1)
		current := active.Add(1)
		for {
			seen := maximum.Load()
			if current <= seen || maximum.CompareAndSwap(seen, current) {
				break
			}
		}
		defer active.Add(-1)
		<-call.Done()
		return nil, call.CheckCancelled()
	}); err != nil {
		t.Fatal(err)
	}
	var input strings.Builder
	for id := 1; id <= mcpMaxCalls+1; id++ {
		input.WriteString(mcpCallFrame(fmt.Sprint(id), "example.wait", `{}`, ""))
		input.WriteByte('\n')
	}
	var output bytes.Buffer
	if err := app.Serve(strings.NewReader(input.String()), &output); err != nil {
		t.Fatal(err)
	}
	frames := decodeMCPFrames(t, output.Bytes())
	if len(frames) != 1 {
		t.Fatalf("queue responses = %#v", frames)
	}
	if frames[0]["error"].(map[string]any)["code"].(json.Number).String() != "-32000" {
		t.Fatalf("queue error = %#v", frames[0])
	}
	if starts.Load() != 1 || maximum.Load() != 1 {
		t.Fatalf("starts=%d maximum=%d", starts.Load(), maximum.Load())
	}
}

func TestMCPToolArgBindingIsCLIMetadata(t *testing.T) {
	path := writeMCPTestManifest(t, `{
		"name":"example.run",
		"summary":{"en":"Run the example"},
		"args":[
			{"name":"message","kind":"text","binding":"positional"},
			{"name":"loud","kind":"bool","binding":"flag"}
		]
	}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatalf("manifest with CLI binding metadata failed to load: %v", err)
	}
	if err := app.Bind("example.run", func(map[string]any, *MCPCall) (any, error) { return nil, nil }); err != nil {
		t.Fatal(err)
	}
	frames := serveMCPFrames(t, app, `{"jsonrpc":"2.0","id":"list","method":"tools/list"}`)
	list := frameByID(t, frames, `"list"`)["result"].(map[string]any)
	tool := list["tools"].([]any)[0].(map[string]any)
	schemaBytes, err := json.Marshal(tool["inputSchema"])
	if err != nil {
		t.Fatal(err)
	}
	// `binding` is one-shot CLI metadata only; it must not leak into the
	// model-facing MCP inputSchema.
	if strings.Contains(string(schemaBytes), "binding") {
		t.Fatalf("binding leaked into input schema: %s", schemaBytes)
	}
	properties := tool["inputSchema"].(map[string]any)["properties"].(map[string]any)
	if _, ok := properties["message"]; !ok {
		t.Fatalf("missing message property: %#v", properties)
	}
	if _, ok := properties["loud"]; !ok {
		t.Fatalf("missing loud property: %#v", properties)
	}
}

func TestMCPManifestValidationAndMissingBindings(t *testing.T) {
	tests := []struct {
		name  string
		tools string
	}{
		{name: "empty tools", tools: ``},
		{name: "duplicate tools", tools: `{"name":"a.run","summary":{"en":"A"}},{"name":"a.run","summary":{"en":"B"}}`},
		{name: "summary must be localized", tools: `{"name":"a.run","summary":"A"}`},
		{name: "unsupported session arg field", tools: `{"name":"a.run","summary":{"en":"A"},"args":[{"name":"x","kind":"text","aliases":["--x"]}]}`},
		{name: "invalid CLI binding", tools: `{"name":"a.run","summary":{"en":"A"},"args":[{"name":"x","kind":"text","binding":"sideways"}]}`},
		{name: "unknown tool field", tools: `{"name":"a.run","summary":{"en":"A"},"unknown":true}`},
		{name: "condition references later arg", tools: `{"name":"a.run","summary":{"en":"A"},"args":[{"name":"x","kind":"text","required_when":{"kind":"arg-present","arg":"y"}},{"name":"y","kind":"text"}]}`},
	}

	for _, body := range []string{
		`{"schema_version":2,"id":"example","version":"1","name":{"en":"Example"},"unknown":true,"mcp":{"tools":[{"name":"a.run","summary":{"en":"A"}}]}}`,
		`{"schema_version":2,"id":"example","version":"1","name":{"en":"Example"},"mcp":{"unknown":true,"tools":[{"name":"a.run","summary":{"en":"A"}}]}}`,
	} {
		if _, err := LoadMCPApp(writeMCPRawManifest(t, body)); err == nil {
			t.Fatal("manifest with unknown field loaded")
		}
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if _, err := LoadMCPApp(writeMCPTestManifest(t, test.tools)); err == nil {
				t.Fatal("invalid manifest loaded")
			}
		})
	}

	path := writeMCPTestManifest(t, `{"name":"a.run","summary":{"en":"A"}}`)
	app, err := LoadMCPApp(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := app.Serve(strings.NewReader(""), io.Discard); err == nil ||
		!strings.Contains(err.Error(), "missing handlers") {
		t.Fatalf("missing binding error = %v", err)
	}

	oversized := filepath.Join(t.TempDir(), "large-app.json")
	if err := os.WriteFile(oversized, bytes.Repeat([]byte{' '}, MaxMCPManifestBytes+1), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadMCPApp(oversized); err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Fatalf("oversized manifest error = %v", err)
	}
}

func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func writePipeFrame(t *testing.T, writer *io.PipeWriter, frame string) {
	t.Helper()
	if _, err := io.WriteString(writer, frame+"\n"); err != nil {
		t.Fatal(err)
	}
}

func waitChannel(t *testing.T, channel <-chan struct{}, label string) {
	t.Helper()
	select {
	case <-channel:
	case <-time.After(2 * time.Second):
		t.Fatalf("timed out waiting for %s", label)
	}
}

func waitError(t *testing.T, channel <-chan error) error {
	t.Helper()
	select {
	case err := <-channel:
		return err
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for Serve")
		return nil
	}
}
