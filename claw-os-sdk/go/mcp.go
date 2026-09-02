package clawossdk

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"math/big"
	"os"
	"regexp"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"
)

const (
	// MCPProtocolVersion is the MCP revision spoken by the App runtime.
	MCPProtocolVersion = "2025-06-18"
	// MCPCallContextMetaKey identifies the Gateway-authenticated call context.
	MCPCallContextMetaKey = "claw-os.dev/call-context"
	// MaxMCPFrameBytes is the maximum newline-delimited inbound frame size.
	MaxMCPFrameBytes = 16 * 1024 * 1024
	// MaxMCPManifestBytes is the maximum app.json size accepted by LoadMCPApp.
	MaxMCPManifestBytes = 1024 * 1024
)

const (
	mcpJSONRPCVersion = "2.0"
	mcpMaxCalls       = 64
	mcpEOFGrace       = 50 * time.Millisecond

	mcpErrParse          = -32700
	mcpErrInvalidRequest = -32600
	mcpErrMethodNotFound = -32601
	mcpErrInvalidParams  = -32602
	mcpErrInternal       = -32603
	mcpErrServerBusy     = -32000
)

var (
	mcpAppIDPattern    = regexp.MustCompile(`^[a-z][a-z0-9_-]*$`)
	mcpToolNamePattern = regexp.MustCompile(`^[a-z][a-z0-9._-]*$`)
)

// MCPManifestError reports an invalid or unreadable authoritative App manifest.
type MCPManifestError struct {
	Message string
}

func (e *MCPManifestError) Error() string { return e.Message }

// MCPCallCancelled is returned by CheckCancelled after cancellation or deadline.
type MCPCallCancelled struct {
	CallID string
	Reason string
}

func (e *MCPCallCancelled) Error() string {
	if e.Reason != "" {
		return e.Reason
	}
	return fmt.Sprintf("call %q was cancelled", e.CallID)
}

// MCPProgress supplies optional details for a progress notification.
type MCPProgress struct {
	Total   *float64
	Message string
}

// MCPCall wraps Go cancellation/deadline semantics and an authenticated,
// immutable snapshot of the generated McpCallContext.
type MCPCall struct {
	ctx           context.Context
	authenticated McpCallContext
	progressToken any
	emitProgress  func(any, float64, MCPProgress) error
}

var _ context.Context = (*MCPCall)(nil)

// Deadline implements context.Context.
func (c *MCPCall) Deadline() (time.Time, bool) { return c.ctx.Deadline() }

// Done implements context.Context.
func (c *MCPCall) Done() <-chan struct{} { return c.ctx.Done() }

// Err implements context.Context.
func (c *MCPCall) Err() error { return c.ctx.Err() }

// Value implements context.Context.
func (c *MCPCall) Value(key any) any { return c.ctx.Value(key) }

// Authenticated returns a copy of the Gateway-authenticated identity and
// lineage. Caller-supplied tool arguments never populate this value.
func (c *MCPCall) Authenticated() McpCallContext {
	return c.authenticated
}

// Caller returns a copy of the authenticated caller principal.
func (c *MCPCall) Caller() McpPrincipal {
	return c.authenticated.Caller
}

// ProgressRequested reports whether the caller supplied a progress token.
func (c *MCPCall) ProgressRequested() bool {
	return c.progressToken != nil
}

// CheckCancelled converts context cancellation or deadline expiry into an
// MCPCallCancelled error with the authenticated call ID.
func (c *MCPCall) CheckCancelled() error {
	if err := c.ctx.Err(); err != nil {
		reason := fmt.Sprintf("call %q was cancelled", c.authenticated.CallId)
		if err == context.DeadlineExceeded {
			reason = fmt.Sprintf("call %q exceeded its deadline", c.authenticated.CallId)
		} else if cause := context.Cause(c.ctx); cause != nil {
			reason = cause.Error()
		}
		return &MCPCallCancelled{CallID: c.authenticated.CallId, Reason: reason}
	}
	return nil
}

// ReportProgress emits notifications/progress when the caller supplied a
// progress token. It is a no-op otherwise.
func (c *MCPCall) ReportProgress(progress float64, options MCPProgress) error {
	if err := c.CheckCancelled(); err != nil {
		return err
	}
	if c.progressToken == nil {
		return nil
	}
	if !validProgressNumber(progress) {
		return fmt.Errorf("progress must be a finite non-negative number")
	}
	if options.Total != nil && !validProgressNumber(*options.Total) {
		return fmt.Errorf("total must be a finite non-negative number")
	}
	return c.emitProgress(c.progressToken, progress, options)
}

// MCPHandler handles one manifest-declared MCP tool call.
type MCPHandler func(args map[string]any, call *MCPCall) (any, error)

// MCPToolResult is an explicit MCP result. Construct values with
// TextMCPResult, ErrorMCPResult, or StructuredMCPResult.
type MCPToolResult struct {
	text       string
	isError    bool
	structured json.RawMessage
}

// TextMCPResult constructs a successful text-only MCP result.
func TextMCPResult(text string) MCPToolResult {
	return MCPToolResult{text: text}
}

// ErrorMCPResult constructs an MCP tool error carried in a successful JSON-RPC
// response.
func ErrorMCPResult(message string) MCPToolResult {
	return MCPToolResult{text: message, isError: true}
}

// StructuredMCPResult constructs a successful object result with both
// structuredContent and text. An empty text value renders the object as JSON.
func StructuredMCPResult(value map[string]any, text string) (MCPToolResult, error) {
	if value == nil {
		return MCPToolResult{}, fmt.Errorf("structured MCP content must be an object")
	}
	raw, err := json.Marshal(value)
	if err != nil {
		return MCPToolResult{}, fmt.Errorf("encode structured MCP result: %w", err)
	}
	var snapshot map[string]any
	if err := decodeMCPJSON(raw, &snapshot); err != nil {
		return MCPToolResult{}, fmt.Errorf("decode structured MCP result: %w", err)
	}
	raw, err = json.Marshal(snapshot)
	if err != nil {
		return MCPToolResult{}, fmt.Errorf("encode structured MCP result snapshot: %w", err)
	}
	if text == "" {
		text = string(raw)
	}
	return MCPToolResult{text: text, structured: raw}, nil
}

type mcpCondition struct {
	kind     string
	arg      string
	value    any
	hasValue bool
}

type mcpArgument struct {
	name         string
	kind         string
	required     bool
	repeatable   bool
	choices      []any
	hasDefault   bool
	defaultValue any
	requiredWhen *mcpCondition
	label        string
}

type mcpToolDefinition struct {
	name        string
	summary     string
	args        []mcpArgument
	inputSchema map[string]any
	handler     MCPHandler
}

// MCPApp is a manifest-bound MCP App service.
type MCPApp struct {
	mu      sync.Mutex
	id      string
	version string
	tools   []*mcpToolDefinition
	byName  map[string]*mcpToolDefinition
	serving bool
}

// ID returns the manifest App identity.
func (a *MCPApp) ID() string { return a.id }

// Version returns the manifest App version.
func (a *MCPApp) Version() string { return a.version }

// LoadMCPApp loads the authoritative app.json.mcp service. An empty path uses
// COS_APP_MANIFEST and then app.json for direct development.
func LoadMCPApp(path string) (*MCPApp, error) {
	if path == "" {
		path = os.Getenv("COS_APP_MANIFEST")
		if path == "" {
			path = "app.json"
		}
	}
	raw, err := readMCPManifest(path)
	if err != nil {
		return nil, err
	}
	if !utf8.Valid(raw) {
		return nil, &MCPManifestError{Message: fmt.Sprintf("invalid App manifest %q: not valid UTF-8", path)}
	}
	var manifest map[string]any
	if err := decodeMCPJSON(raw, &manifest); err != nil {
		return nil, &MCPManifestError{Message: fmt.Sprintf("invalid App manifest %q: %v", path, err)}
	}
	return parseMCPManifest(manifest)
}

// Bind attaches a handler to a tool already declared in app.json.mcp.tools.
func (a *MCPApp) Bind(name string, handler MCPHandler) error {
	if handler == nil {
		return fmt.Errorf("MCP tool handler must not be nil")
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.serving {
		return fmt.Errorf("cannot bind MCP tools while serving")
	}
	tool := a.byName[name]
	if tool == nil {
		return fmt.Errorf("tool %q is not declared in app.json.mcp.tools", name)
	}
	if tool.handler != nil {
		return fmt.Errorf("tool %q is already bound", name)
	}
	tool.handler = handler
	return nil
}

// Serve handles newline-delimited MCP JSON-RPC 2.0 over reader and writer.
func (a *MCPApp) Serve(reader io.Reader, writer io.Writer) error {
	if reader == nil || writer == nil {
		return fmt.Errorf("MCP reader and writer must not be nil")
	}
	a.mu.Lock()
	if a.serving {
		a.mu.Unlock()
		return fmt.Errorf("this MCP App is already serving")
	}
	missing := make([]string, 0)
	for _, tool := range a.tools {
		if tool.handler == nil {
			missing = append(missing, tool.name)
		}
	}
	if len(missing) > 0 {
		a.mu.Unlock()
		return &MCPManifestError{Message: "missing handlers for manifest tools: " + strings.Join(missing, ", ")}
	}
	a.serving = true
	a.mu.Unlock()
	defer func() {
		a.mu.Lock()
		a.serving = false
		a.mu.Unlock()
	}()

	runtime := newMCPRuntime(a, reader, writer)
	workerDone := make(chan struct{})
	go func() {
		runtime.runCalls()
		close(workerDone)
	}()

	readErr := runtime.readFrames()
	close(runtime.pending)
	if readErr != nil {
		runtime.cancelAll("MCP input failed", true)
		<-workerDone
	} else if runtime.callCount() != 0 {
		timer := time.NewTimer(mcpEOFGrace)
		select {
		case <-workerDone:
			if !timer.Stop() {
				<-timer.C
			}
		case <-timer.C:
			runtime.cancelAll("MCP input closed", true)
			<-workerDone
		}
	} else {
		<-workerDone
	}

	if outputErr := runtime.output.Err(); outputErr != nil {
		return fmt.Errorf("MCP output failed: %w", outputErr)
	}
	if readErr != nil {
		return fmt.Errorf("MCP input failed: %w", readErr)
	}
	return nil
}

// ServeStdio serves MCP over the process standard streams.
func (a *MCPApp) ServeStdio() error {
	return a.Serve(os.Stdin, os.Stdout)
}

type mcpCallState struct {
	key      string
	id       any
	params   map[string]any
	ctx      context.Context
	cancel   context.CancelCauseFunc
	suppress atomic.Bool
}

type mcpRuntime struct {
	app     *MCPApp
	reader  io.Reader
	closer  io.Closer
	output  *mcpFrameWriter
	root    context.Context
	cancel  context.CancelCauseFunc
	pending chan *mcpCallState

	callsMu sync.Mutex
	calls   map[string]*mcpCallState
}

func newMCPRuntime(app *MCPApp, reader io.Reader, writer io.Writer) *mcpRuntime {
	root, cancel := context.WithCancelCause(context.Background())
	runtime := &mcpRuntime{
		app:     app,
		reader:  reader,
		root:    root,
		cancel:  cancel,
		pending: make(chan *mcpCallState, mcpMaxCalls),
		calls:   make(map[string]*mcpCallState),
	}
	if closer, ok := reader.(io.Closer); ok {
		runtime.closer = closer
	}
	runtime.output = newMCPFrameWriter(writer, func(err error) {
		runtime.cancel(err)
		runtime.cancelAll("MCP output failed", true)
		if runtime.closer != nil {
			_ = runtime.closer.Close()
		}
	})
	return runtime
}

func (r *mcpRuntime) readFrames() error {
	buffered := bufio.NewReaderSize(r.reader, 64*1024)
	for {
		frame, overflowed, err := readMCPFrame(buffered, MaxMCPFrameBytes)
		if err != nil {
			if err == io.EOF {
				return nil
			}
			if outputErr := r.output.Err(); outputErr != nil {
				return nil
			}
			return err
		}
		if overflowed {
			if err := r.sendError(nil, mcpErrParse, fmt.Sprintf("frame exceeds %d bytes; rejected", MaxMCPFrameBytes), nil); err != nil {
				return nil
			}
			continue
		}
		if len(bytes.TrimSpace(frame)) == 0 {
			continue
		}
		if err := r.handleFrame(frame); err != nil {
			return nil
		}
	}
}

func (r *mcpRuntime) handleFrame(frame []byte) error {
	if !utf8.Valid(frame) {
		return r.sendError(nil, mcpErrParse, "frame is not valid UTF-8", nil)
	}
	var decoded any
	if err := decodeMCPJSON(frame, &decoded); err != nil {
		return r.sendError(nil, mcpErrParse, "parse error: "+err.Error(), nil)
	}
	message, ok := decoded.(map[string]any)
	if !ok {
		return r.sendError(nil, mcpErrInvalidRequest, "request not an object", nil)
	}
	rawID, hasID := message["id"]
	if hasID && !validMCPRequestID(rawID) {
		return r.sendError(nil, mcpErrInvalidRequest, "request id must be a string, number, or null", nil)
	}
	id := any(nil)
	if hasID {
		id = rawID
	}
	if message["jsonrpc"] != mcpJSONRPCVersion {
		return r.sendError(id, mcpErrInvalidRequest, "missing jsonrpc 2.0 envelope", nil)
	}
	method, ok := message["method"].(string)
	if !ok {
		return r.sendError(id, mcpErrInvalidRequest, "request method must be a string", nil)
	}
	params, paramsPresent := message["params"]
	if !hasID {
		if paramsPresent && !isMCPObject(params) {
			if _, ok := params.([]any); !ok {
				return r.sendError(nil, mcpErrInvalidRequest, "request params must be an object or array", nil)
			}
		}
		r.handleNotification(method, params)
		return nil
	}
	if method == "tools/call" {
		return r.queueToolCall(id, params)
	}
	result, rpcErr := r.handleRequest(method, params, paramsPresent)
	if rpcErr != nil {
		return r.sendError(id, rpcErr.code, rpcErr.message, rpcErr.data)
	}
	return r.sendResult(id, result)
}

func (r *mcpRuntime) handleNotification(method string, params any) {
	switch method {
	case "notifications/initialized", "notifications/progress":
		return
	case "notifications/cancelled":
		object, ok := params.(map[string]any)
		if !ok {
			return
		}
		requestID, ok := object["requestId"]
		if !ok || !validMCPRequestID(requestID) {
			return
		}
		key, err := mcpRequestKey(requestID)
		if err != nil {
			return
		}
		r.callsMu.Lock()
		state := r.calls[key]
		r.callsMu.Unlock()
		if state != nil {
			state.suppress.Store(true)
			state.cancel(&MCPCallCancelled{Reason: fmt.Sprintf("call %q was cancelled", key)})
		}
	}
}

type mcpRPCError struct {
	code    int
	message string
	data    any
}

func (r *mcpRuntime) handleRequest(method string, params any, paramsPresent bool) (any, *mcpRPCError) {
	switch method {
	case "initialize":
		object, ok := params.(map[string]any)
		if !ok {
			return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "initialize params must be an object"}
		}
		return r.initialize(object)
	case "ping":
		if paramsPresent && !isMCPObject(params) {
			return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "ping params must be an object"}
		}
		return map[string]any{}, nil
	case "tools/list":
		if paramsPresent {
			object, ok := params.(map[string]any)
			if !ok {
				return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "tools/list params must be an object"}
			}
			if cursor, present := object["cursor"]; present {
				if _, ok := cursor.(string); !ok {
					return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "tools/list cursor must be a string"}
				}
			}
		}
		tools := make([]any, 0, len(r.app.tools))
		for _, tool := range r.app.tools {
			tools = append(tools, map[string]any{
				"name":        tool.name,
				"description": tool.summary,
				"inputSchema": tool.inputSchema,
			})
		}
		return map[string]any{"tools": tools}, nil
	default:
		return nil, &mcpRPCError{code: mcpErrMethodNotFound, message: fmt.Sprintf("unknown method %q", method)}
	}
}

func (r *mcpRuntime) initialize(params map[string]any) (any, *mcpRPCError) {
	if _, ok := params["protocolVersion"].(string); !ok {
		return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "missing `protocolVersion`"}
	}
	capabilities, ok := params["capabilities"].(map[string]any)
	if !ok {
		return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "missing `capabilities`"}
	}
	for _, name := range []string{"experimental", "sampling", "elicitation"} {
		if value, present := capabilities[name]; present && !isMCPObject(value) {
			return nil, &mcpRPCError{code: mcpErrInvalidParams, message: fmt.Sprintf("`capabilities.%s` must be an object", name)}
		}
	}
	if value, present := capabilities["roots"]; present {
		roots, ok := value.(map[string]any)
		if !ok {
			return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "`capabilities.roots` must be an object"}
		}
		if listChanged, present := roots["listChanged"]; present {
			if _, ok := listChanged.(bool); !ok {
				return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "`capabilities.roots.listChanged` must be a boolean"}
			}
		}
	}
	clientInfo, ok := params["clientInfo"].(map[string]any)
	if !ok {
		return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "missing or invalid `clientInfo`"}
	}
	if _, ok := clientInfo["name"].(string); !ok {
		return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "missing or invalid `clientInfo`"}
	}
	if _, ok := clientInfo["version"].(string); !ok {
		return nil, &mcpRPCError{code: mcpErrInvalidParams, message: "missing or invalid `clientInfo`"}
	}
	return map[string]any{
		"protocolVersion": MCPProtocolVersion,
		"capabilities": map[string]any{
			"tools": map[string]any{"listChanged": false},
		},
		"serverInfo": map[string]any{"name": r.app.id, "version": r.app.version},
	}, nil
}

func (r *mcpRuntime) queueToolCall(id any, params any) error {
	object, ok := params.(map[string]any)
	if !ok {
		return r.sendError(id, mcpErrInvalidParams, "tools/call params must be an object", nil)
	}
	key, err := mcpRequestKey(id)
	if err != nil {
		return r.sendError(nil, mcpErrInvalidRequest, err.Error(), nil)
	}
	r.callsMu.Lock()
	if len(r.calls) >= mcpMaxCalls {
		r.callsMu.Unlock()
		return r.sendError(id, mcpErrServerBusy, "too many pending MCP tool calls", nil)
	}
	if _, exists := r.calls[key]; exists {
		r.callsMu.Unlock()
		return r.sendError(id, mcpErrInvalidRequest, "duplicate active request id", nil)
	}
	callContext, cancel := context.WithCancelCause(r.root)
	state := &mcpCallState{
		key:    key,
		id:     id,
		params: object,
		ctx:    callContext,
		cancel: cancel,
	}
	r.calls[key] = state
	r.callsMu.Unlock()
	r.pending <- state
	return nil
}

func (r *mcpRuntime) runCalls() {
	for state := range r.pending {
		if !state.suppress.Load() {
			r.executeToolCall(state)
		}
		state.cancel(&MCPCallCancelled{Reason: "MCP call completed"})
		r.callsMu.Lock()
		delete(r.calls, state.key)
		r.callsMu.Unlock()
	}
}

func (r *mcpRuntime) executeToolCall(state *mcpCallState) {
	name, ok := state.params["name"].(string)
	if !ok {
		r.sendCallError(state, mcpErrInvalidParams, "missing `name`")
		return
	}
	tool := r.app.byName[name]
	if tool == nil {
		r.sendCallError(state, mcpErrInvalidParams, fmt.Sprintf("unknown tool %q", name))
		return
	}
	supplied := map[string]any{}
	if value, present := state.params["arguments"]; present {
		object, ok := value.(map[string]any)
		if !ok {
			r.sendCallError(state, mcpErrInvalidParams, "`arguments` must be an object")
			return
		}
		supplied = object
	}
	call, cleanup, rpcErr := r.makeCall(state)
	if rpcErr != nil {
		r.sendCallError(state, rpcErr.code, rpcErr.message)
		return
	}
	defer cleanup()
	args, err := resolveMCPArguments(tool, supplied)
	if err != nil {
		r.sendCallResult(state, mcpToolError(fmt.Sprintf("bad arguments for %q: %v", name, err)))
		return
	}
	if err := call.CheckCancelled(); err != nil {
		if !state.suppress.Load() {
			r.sendCallResult(state, mcpToolError(err.Error()))
		}
		return
	}
	value, handlerErr := invokeMCPHandler(tool.handler, args, call)
	if state.suppress.Load() {
		return
	}
	if handlerErr != nil {
		r.sendCallResult(state, mcpToolError(handlerErr.Error()))
		return
	}
	if err := call.CheckCancelled(); err != nil {
		r.sendCallResult(state, mcpToolError(err.Error()))
		return
	}
	result, err := coerceMCPResult(value)
	if err != nil {
		result = mcpToolError("invalid tool result: " + err.Error())
	}
	r.sendCallResult(state, result)
}

func invokeMCPHandler(handler MCPHandler, args map[string]any, call *MCPCall) (value any, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("MCP tool handler panicked: %v", recovered)
		}
	}()
	return handler(args, call)
}

func (r *mcpRuntime) makeCall(state *mcpCallState) (*MCPCall, context.CancelFunc, *mcpRPCError) {
	meta, ok := state.params["_meta"].(map[string]any)
	if !ok {
		return nil, nil, &mcpRPCError{code: mcpErrInvalidParams, message: "`_meta` must be an object"}
	}
	progressToken, progressPresent := meta["progressToken"]
	if progressPresent && !validMCPProgressToken(progressToken) {
		return nil, nil, &mcpRPCError{code: mcpErrInvalidParams, message: "`_meta.progressToken` must be a string or integer"}
	}
	rawContext, ok := meta[MCPCallContextMetaKey]
	if !ok {
		return nil, nil, &mcpRPCError{code: mcpErrInvalidParams, message: fmt.Sprintf("missing authenticated %q", MCPCallContextMetaKey)}
	}
	if err := ValidateMcpCallContext(rawContext); err != nil {
		return nil, nil, &mcpRPCError{code: mcpErrInvalidParams, message: "invalid authenticated call context: " + err.Error()}
	}
	authenticated := materializeMCPCallContext(rawContext.(map[string]any))
	callContext := state.ctx
	cleanup := context.CancelFunc(func() {})
	if authenticated.DeadlineUnixMs != 0 {
		deadline := time.UnixMilli(int64(authenticated.DeadlineUnixMs))
		callContext, cleanup = context.WithDeadline(callContext, deadline)
	}

	return &MCPCall{
		ctx:           callContext,
		authenticated: authenticated,
		progressToken: progressToken,
		emitProgress: func(token any, progress float64, options MCPProgress) error {
			params := map[string]any{
				"progressToken": token,
				"progress":      progress,
			}
			if options.Total != nil {
				params["total"] = *options.Total
			}
			if options.Message != "" {
				params["message"] = options.Message
			}
			return r.sendNotification("notifications/progress", params)
		},
	}, cleanup, nil
}

func materializeMCPCallContext(raw map[string]any) McpCallContext {
	caller := raw["caller"].(map[string]any)
	return McpCallContext{
		WireVersion:    int(asUint64(raw["wire_version"])),
		CallId:         raw["call_id"].(string),
		TraceId:        raw["trace_id"].(string),
		ParentCallId:   asString(raw["parent_call_id"]),
		Depth:          asUint64(raw["depth"]),
		DeadlineUnixMs: asUint64(raw["deadline_unix_ms"]),
		SessionId:      asString(raw["session_id"]),
		TaskId:         asString(raw["task_id"]),
		Caller: McpPrincipal{
			Kind:     caller["kind"].(string),
			Id:       caller["id"].(string),
			OwnerUid: asUint64(caller["owner_uid"]),
			AppId:    asString(caller["app_id"]),
		},
	}
}

func (r *mcpRuntime) sendCallResult(state *mcpCallState, result any) {
	if state.suppress.Load() {
		return
	}
	_ = r.sendResult(state.id, result)
}

func (r *mcpRuntime) sendCallError(state *mcpCallState, code int, message string) {
	if state.suppress.Load() {
		return
	}
	_ = r.sendError(state.id, code, message, nil)
}

func (r *mcpRuntime) sendResult(id, result any) error {
	return r.output.Write(map[string]any{
		"jsonrpc": mcpJSONRPCVersion,
		"id":      id,
		"result":  result,
	})
}

func (r *mcpRuntime) sendError(id any, code int, message string, data any) error {
	body := map[string]any{"code": code, "message": message}
	if data != nil {
		body["data"] = data
	}
	return r.output.Write(map[string]any{
		"jsonrpc": mcpJSONRPCVersion,
		"id":      id,
		"error":   body,
	})
}

func (r *mcpRuntime) sendNotification(method string, params map[string]any) error {
	return r.output.Write(map[string]any{
		"jsonrpc": mcpJSONRPCVersion,
		"method":  method,
		"params":  params,
	})
}

func (r *mcpRuntime) cancelAll(reason string, suppress bool) {
	r.callsMu.Lock()
	states := make([]*mcpCallState, 0, len(r.calls))
	for _, state := range r.calls {
		states = append(states, state)
	}
	r.callsMu.Unlock()
	for _, state := range states {
		if suppress {
			state.suppress.Store(true)
		}
		state.cancel(&MCPCallCancelled{Reason: reason})
	}
}

func (r *mcpRuntime) callCount() int {
	r.callsMu.Lock()
	defer r.callsMu.Unlock()
	return len(r.calls)
}

type mcpFrameWriter struct {
	mu        sync.Mutex
	writer    io.Writer
	err       error
	onFailure func(error)
}

func newMCPFrameWriter(writer io.Writer, onFailure func(error)) *mcpFrameWriter {
	return &mcpFrameWriter{writer: writer, onFailure: onFailure}
}

func (w *mcpFrameWriter) Write(frame any) error {
	encoded, err := json.Marshal(frame)
	if err != nil {
		return w.fail(err)
	}
	encoded = append(encoded, '\n')
	w.mu.Lock()
	if w.err != nil {
		err := w.err
		w.mu.Unlock()
		return err
	}
	for len(encoded) > 0 {
		n, writeErr := w.writer.Write(encoded)
		if writeErr != nil {
			w.err = writeErr
			w.mu.Unlock()
			w.onFailure(writeErr)
			return writeErr
		}
		if n <= 0 {
			w.err = io.ErrShortWrite
			w.mu.Unlock()
			w.onFailure(io.ErrShortWrite)
			return io.ErrShortWrite
		}
		encoded = encoded[n:]
	}
	w.mu.Unlock()
	return nil
}

func (w *mcpFrameWriter) fail(err error) error {
	w.mu.Lock()
	first := w.err == nil
	if w.err == nil {
		w.err = err
	}
	stored := w.err
	w.mu.Unlock()
	if first {
		w.onFailure(err)
	}
	return stored
}

func (w *mcpFrameWriter) Err() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.err
}

func readMCPManifest(path string) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, &MCPManifestError{Message: fmt.Sprintf("cannot read App manifest %q: %v", path, err)}
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, MaxMCPManifestBytes+1))
	if err != nil {
		return nil, &MCPManifestError{Message: fmt.Sprintf("cannot read App manifest %q: %v", path, err)}
	}
	if len(raw) > MaxMCPManifestBytes {
		return nil, &MCPManifestError{Message: fmt.Sprintf("App manifest %q exceeds %d bytes", path, MaxMCPManifestBytes)}
	}
	return raw, nil
}

func parseMCPManifest(manifest map[string]any) (*MCPApp, error) {
	if !mcpIntegerEquals(manifest["schema_version"], 2) {
		return nil, &MCPManifestError{Message: "MCP Apps require `schema_version: 2`"}
	}
	id, ok := manifest["id"].(string)
	if !ok || !mcpAppIDPattern.MatchString(id) {
		return nil, &MCPManifestError{Message: "App manifest has no valid `id`"}
	}
	version, ok := manifest["version"].(string)
	if !ok || strings.TrimSpace(version) == "" {
		return nil, &MCPManifestError{Message: "App manifest has no valid `version`"}
	}
	service, ok := manifest["mcp"].(map[string]any)
	if !ok {
		return nil, &MCPManifestError{Message: "App manifest has no `mcp` service"}
	}
	if transport, present := service["transport"]; present && transport != "stdio" {
		return nil, &MCPManifestError{Message: "`mcp.transport` must be `stdio`"}
	}
	rawTools := []any{}
	if value, present := service["tools"]; present {
		var ok bool
		rawTools, ok = value.([]any)
		if !ok {
			return nil, &MCPManifestError{Message: "`mcp.tools` must be an array"}
		}
	}
	app := &MCPApp{
		id:      id,
		version: version,
		tools:   make([]*mcpToolDefinition, 0, len(rawTools)),
		byName:  make(map[string]*mcpToolDefinition, len(rawTools)),
	}
	for index, rawTool := range rawTools {
		object, ok := rawTool.(map[string]any)
		if !ok {
			return nil, &MCPManifestError{Message: fmt.Sprintf("`mcp.tools[%d]` must be an object", index)}
		}
		name, ok := object["name"].(string)
		if !ok || !mcpToolNamePattern.MatchString(name) {
			return nil, &MCPManifestError{Message: fmt.Sprintf("`mcp.tools[%d].name` is invalid", index)}
		}
		if app.byName[name] != nil {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q is declared twice", name)}
		}
		summary, err := mcpLocalizedEnglish(object["summary"], fmt.Sprintf("mcp.tools[%d].summary", index))
		if err != nil {
			return nil, err
		}
		rawArgs := []any{}
		if value, present := object["args"]; present {
			var ok bool
			rawArgs, ok = value.([]any)
			if !ok {
				return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q args must be an array", name)}
			}
		}
		args, err := parseMCPArguments(name, rawArgs)
		if err != nil {
			return nil, err
		}
		tool := &mcpToolDefinition{
			name:        name,
			summary:     summary,
			args:        args,
			inputSchema: buildMCPInputSchema(args),
		}
		app.tools = append(app.tools, tool)
		app.byName[name] = tool
	}
	return app, nil
}

func parseMCPArguments(toolName string, rawArgs []any) ([]mcpArgument, error) {
	args := make([]mcpArgument, 0, len(rawArgs))
	earlier := make(map[string]bool, len(rawArgs))
	for index, raw := range rawArgs {
		object, ok := raw.(map[string]any)
		if !ok {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %d must be an object", toolName, index)}
		}
		name, ok := object["name"].(string)
		if !ok || strings.TrimSpace(name) == "" {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %d has no valid name", toolName, index)}
		}
		if earlier[name] {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q is duplicated", toolName, name)}
		}
		kind, ok := object["kind"].(string)
		if !ok || !validMCPArgKind(kind) {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q has invalid kind", toolName, name)}
		}
		for _, unsupported := range []string{"default_from", "trusted_resolver", "aliases", "positional_alias"} {
			if _, present := object[unsupported]; present {
				return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q cannot declare %q", toolName, name, unsupported)}
			}
		}
		required, err := optionalMCPBool(object, "required", toolName, name)
		if err != nil {
			return nil, err
		}
		repeatable, err := optionalMCPBool(object, "repeatable", toolName, name)
		if err != nil {
			return nil, err
		}
		if binding, present := object["binding"]; present && binding != "positional" && binding != "flag" {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q has invalid binding", toolName, name)}
		}
		if repeatable && kind == "bool" {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q cannot repeat booleans", toolName, name)}
		}
		choices := []any{}
		if rawChoices, present := object["choices"]; present {
			var ok bool
			choices, ok = rawChoices.([]any)
			if !ok {
				return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q choices must be an array", toolName, name)}
			}
		}
		for choiceIndex, choice := range choices {
			if err := validateMCPScalar(name, kind, choice); err != nil {
				return nil, &MCPManifestError{Message: fmt.Sprintf("choice %d for %q: %v", choiceIndex, name, err)}
			}
			for prior := 0; prior < choiceIndex; prior++ {
				if mcpValuesEqual(choice, choices[prior]) {
					return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q choices must be unique", toolName, name)}
				}
			}
		}
		defaultValue, hasDefault := object["default"]
		if required && hasDefault {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q cannot be required and defaulted", toolName, name)}
		}
		var condition *mcpCondition
		if rawCondition, present := object["required_when"]; present {
			condition, err = parseMCPCondition(toolName, name, rawCondition, earlier)
			if err != nil {
				return nil, err
			}
			if required || repeatable || hasDefault {
				return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q has an incompatible required_when declaration", toolName, name)}
			}
		}
		if hasDefault {
			if repeatable {
				values, ok := defaultValue.([]any)
				if !ok {
					return nil, &MCPManifestError{Message: fmt.Sprintf("default for %q must be an array", name)}
				}
				for _, value := range values {
					if err := validateMCPScalar(name, kind, value); err != nil {
						return nil, &MCPManifestError{Message: "default " + err.Error()}
					}
					if len(choices) > 0 && !mcpContains(choices, value) {
						return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q default is not an allowed choice", toolName, name)}
					}
				}
			} else {
				if err := validateMCPScalar(name, kind, defaultValue); err != nil {
					return nil, &MCPManifestError{Message: "default " + err.Error()}
				}
				if len(choices) > 0 && !mcpContains(choices, defaultValue) {
					return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q default is not an allowed choice", toolName, name)}
				}
			}
		}
		label := ""
		if rawLabel, present := object["label"]; present {
			label, err = mcpLocalizedEnglish(rawLabel, fmt.Sprintf("tool %q arg %q label", toolName, name))
			if err != nil {
				return nil, err
			}
		}
		args = append(args, mcpArgument{
			name:         name,
			kind:         kind,
			required:     required,
			repeatable:   repeatable,
			choices:      choices,
			hasDefault:   hasDefault,
			defaultValue: defaultValue,
			requiredWhen: condition,
			label:        label,
		})
		earlier[name] = true
	}
	return args, nil
}

func parseMCPCondition(toolName, argName string, raw any, earlier map[string]bool) (*mcpCondition, error) {
	object, ok := raw.(map[string]any)
	if !ok {
		return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q required_when must be an object", toolName, argName)}
	}
	allowed := map[string]bool{"kind": true, "arg": true, "value": true}
	for field := range object {
		if !allowed[field] {
			return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q required_when has unknown field %q", toolName, argName, field)}
		}
	}
	kind, _ := object["kind"].(string)
	if kind != "arg-present" && kind != "arg-equals" && kind != "arg-not-equals" {
		return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q has invalid required_when kind", toolName, argName)}
	}
	source, ok := object["arg"].(string)
	if !ok || !earlier[source] {
		return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q required_when must reference an earlier arg", toolName, argName)}
	}
	value, hasValue := object["value"]
	if kind == "arg-present" && hasValue {
		return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q arg-present cannot declare value", toolName, argName)}
	}
	if kind != "arg-present" && (!hasValue || value == nil) {
		return nil, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q condition requires a non-null value", toolName, argName)}
	}
	return &mcpCondition{kind: kind, arg: source, value: value, hasValue: hasValue}, nil
}

func buildMCPInputSchema(args []mcpArgument) map[string]any {
	properties := make(map[string]any, len(args))
	required := make([]string, 0)
	allOf := make([]any, 0)
	for _, arg := range args {
		scalar := map[string]any{"type": mcpJSONType(arg.kind)}
		if len(arg.choices) > 0 {
			scalar["enum"] = arg.choices
		}
		property := any(scalar)
		if arg.repeatable {
			property = map[string]any{"type": "array", "items": scalar}
		}
		propertyMap := property.(map[string]any)
		if arg.label != "" {
			propertyMap["description"] = arg.label
		}
		if arg.hasDefault {
			propertyMap["default"] = arg.defaultValue
		}
		properties[arg.name] = propertyMap
		if arg.required {
			required = append(required, arg.name)
		}
		if arg.requiredWhen != nil {
			allOf = append(allOf, map[string]any{
				"if":   mcpConditionSchema(arg.requiredWhen),
				"then": map[string]any{"required": []string{arg.name}},
				"else": map[string]any{"not": map[string]any{"required": []string{arg.name}}},
			})
		}
	}
	schema := map[string]any{
		"type":                 "object",
		"properties":           properties,
		"additionalProperties": false,
	}
	if len(required) > 0 {
		schema["required"] = required
	}
	if len(allOf) > 0 {
		schema["allOf"] = allOf
	}
	return schema
}

func mcpConditionSchema(condition *mcpCondition) map[string]any {
	switch condition.kind {
	case "arg-present":
		return map[string]any{"required": []string{condition.arg}}
	case "arg-equals":
		return map[string]any{
			"properties": map[string]any{condition.arg: map[string]any{"const": condition.value}},
			"required":   []string{condition.arg},
		}
	default:
		return map[string]any{
			"required": []string{condition.arg},
			"not": map[string]any{
				"properties": map[string]any{condition.arg: map[string]any{"const": condition.value}},
				"required":   []string{condition.arg},
			},
		}
	}
}

func resolveMCPArguments(tool *mcpToolDefinition, supplied map[string]any) (map[string]any, error) {
	declared := make(map[string]mcpArgument, len(tool.args))
	for _, arg := range tool.args {
		declared[arg.name] = arg
	}
	extras := make([]string, 0)
	for name := range supplied {
		if _, ok := declared[name]; !ok {
			extras = append(extras, name)
		}
	}
	if len(extras) > 0 {
		sort.Strings(extras)
		return nil, fmt.Errorf("unknown argument %q", extras[0])
	}
	resolved := make(map[string]any, len(supplied)+len(tool.args))
	for name, value := range supplied {
		resolved[name] = value
	}
	for _, arg := range tool.args {
		active := arg.requiredWhen == nil || mcpConditionMatches(arg.requiredWhen, resolved)
		value, present := resolved[arg.name]
		if !active {
			if present {
				return nil, fmt.Errorf("%q is not accepted when its condition is false", arg.name)
			}
			continue
		}
		if !present {
			if arg.hasDefault {
				cloned, err := cloneMCPJSON(arg.defaultValue)
				if err != nil {
					return nil, fmt.Errorf("clone default for %q: %w", arg.name, err)
				}
				value = cloned
				resolved[arg.name] = value
			} else if arg.required || arg.requiredWhen != nil {
				return nil, fmt.Errorf("missing required argument %q", arg.name)
			} else {
				continue
			}
		}
		if arg.repeatable {
			values, ok := value.([]any)
			if !ok {
				return nil, fmt.Errorf("%q must be an array", arg.name)
			}
			for _, item := range values {
				if err := validateMCPCallScalar(arg, item); err != nil {
					return nil, err
				}
			}
		} else if err := validateMCPCallScalar(arg, value); err != nil {
			return nil, err
		}
	}
	return resolved, nil
}

func validateMCPCallScalar(arg mcpArgument, value any) error {
	if err := validateMCPScalar(arg.name, arg.kind, value); err != nil {
		return err
	}
	if len(arg.choices) > 0 && !mcpContains(arg.choices, value) {
		return fmt.Errorf("%q is not one of its allowed values", arg.name)
	}
	return nil
}

func validateMCPScalar(name, kind string, value any) error {
	switch kind {
	case "path", "host", "name", "text":
		if _, ok := value.(string); !ok {
			return fmt.Errorf("%q must be a string", name)
		}
	case "bool":
		if _, ok := value.(bool); !ok {
			return fmt.Errorf("%q must be a boolean", name)
		}
	case "integer":
		if _, ok := wireExactInteger(value); !ok {
			return fmt.Errorf("%q must be an integer", name)
		}
	case "number":
		if !wireNumberValid(value) {
			return fmt.Errorf("%q must be a number", name)
		}
	default:
		return fmt.Errorf("%q has unsupported kind %q", name, kind)
	}
	return nil
}

func mcpConditionMatches(condition *mcpCondition, values map[string]any) bool {
	value, present := values[condition.arg]
	switch condition.kind {
	case "arg-present":
		return present
	case "arg-equals":
		return present && mcpValuesEqual(value, condition.value)
	default:
		return present && !mcpValuesEqual(value, condition.value)
	}
}

func coerceMCPResult(value any) (map[string]any, error) {
	switch result := value.(type) {
	case MCPToolResult:
		return mcpExplicitResult(result)
	case *MCPToolResult:
		if result == nil {
			return mcpTextResult(""), nil
		}
		return mcpExplicitResult(*result)
	case map[string]any:
		explicit, err := StructuredMCPResult(result, "")
		if err != nil {
			return nil, err
		}
		return mcpExplicitResult(explicit)
	case string:
		return mcpTextResult(result), nil
	case nil:
		return mcpTextResult(""), nil
	default:
		raw, err := json.Marshal(value)
		if err != nil {
			return nil, err
		}
		var decoded any
		if err := decodeMCPJSON(raw, &decoded); err != nil {
			return nil, err
		}
		if object, ok := decoded.(map[string]any); ok {
			explicit, err := StructuredMCPResult(object, string(raw))
			if err != nil {
				return nil, err
			}
			return mcpExplicitResult(explicit)
		}
		return mcpTextResult(string(raw)), nil
	}
}

func mcpExplicitResult(result MCPToolResult) (map[string]any, error) {
	payload := mcpTextResult(result.text)
	payload["isError"] = result.isError
	if len(result.structured) != 0 {
		var structured map[string]any
		if err := decodeMCPJSON(result.structured, &structured); err != nil {
			return nil, err
		}
		payload["structuredContent"] = structured
	}
	return payload, nil
}

func mcpTextResult(text string) map[string]any {
	return map[string]any{
		"content": []any{map[string]any{"type": "text", "text": text}},
		"isError": false,
	}
}

func mcpToolError(message string) map[string]any {
	result := mcpTextResult(message)
	result["isError"] = true
	return result
}

func readMCPFrame(reader *bufio.Reader, limit int) ([]byte, bool, error) {
	var frame []byte
	overflowed := false
	for {
		fragment, err := reader.ReadSlice('\n')
		terminated := len(fragment) > 0 && fragment[len(fragment)-1] == '\n'
		content := fragment
		if terminated {
			content = fragment[:len(fragment)-1]
		}
		if !overflowed {
			if len(frame)+len(content) > limit {
				frame = nil
				overflowed = true
			} else {
				frame = append(frame, content...)
			}
		}
		if terminated {
			if !overflowed && len(frame) > 0 && frame[len(frame)-1] == '\r' {
				frame = frame[:len(frame)-1]
			}
			return frame, overflowed, nil
		}
		if err != nil {
			if err == io.EOF {
				if len(fragment) == 0 && len(frame) == 0 && !overflowed {
					return nil, false, io.EOF
				}
				return frame, overflowed, nil
			}
			if err != bufio.ErrBufferFull {
				return nil, false, err
			}
		}
	}
}

func decodeMCPJSON(raw []byte, target any) error {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(target); err != nil {
		return err
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return fmt.Errorf("trailing JSON data")
		}
		return err
	}
	return nil
}

func cloneMCPJSON(value any) (any, error) {
	raw, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	var cloned any
	if err := decodeMCPJSON(raw, &cloned); err != nil {
		return nil, err
	}
	return cloned, nil
}

func validMCPRequestID(value any) bool {
	switch value.(type) {
	case nil, string:
		return true
	case json.Number:
		return wireNumberValid(value)
	default:
		return false
	}
}

func validMCPProgressToken(value any) bool {
	switch value.(type) {
	case string:
		return true
	case json.Number:
		_, ok := wireExactInteger(value)
		return ok
	default:
		return false
	}
}

func mcpRequestKey(value any) (string, error) {
	if !validMCPRequestID(value) {
		return "", fmt.Errorf("request id must be a string, number, or null")
	}
	raw, err := json.Marshal(value)
	if err != nil {
		return "", err
	}
	return string(raw), nil
}

func validProgressNumber(value float64) bool {
	return !math.IsNaN(value) && !math.IsInf(value, 0) && value >= 0
}

func validMCPArgKind(kind string) bool {
	switch kind {
	case "path", "host", "name", "text", "number", "integer", "bool":
		return true
	default:
		return false
	}
}

func mcpJSONType(kind string) string {
	switch kind {
	case "number":
		return "number"
	case "integer":
		return "integer"
	case "bool":
		return "boolean"
	default:
		return "string"
	}
}

func optionalMCPBool(object map[string]any, field, toolName, argName string) (bool, error) {
	value, present := object[field]
	if !present {
		return false, nil
	}
	boolean, ok := value.(bool)
	if !ok {
		return false, &MCPManifestError{Message: fmt.Sprintf("tool %q arg %q %s must be boolean", toolName, argName, field)}
	}
	return boolean, nil
}

func mcpLocalizedEnglish(value any, field string) (string, error) {
	object, ok := value.(map[string]any)
	if !ok {
		return "", &MCPManifestError{Message: fmt.Sprintf("`%s` requires non-empty English text", field)}
	}
	english, ok := object["en"].(string)
	if !ok || strings.TrimSpace(english) == "" {
		return "", &MCPManifestError{Message: fmt.Sprintf("`%s` requires non-empty English text", field)}
	}
	return english, nil
}

func mcpIntegerEquals(value any, expected int64) bool {
	integer, ok := wireExactInteger(value)
	return ok && integer.overflow == 0 && integer.value.Cmp(big.NewInt(expected)) == 0
}

func mcpContains(values []any, candidate any) bool {
	for _, value := range values {
		if mcpValuesEqual(value, candidate) {
			return true
		}
	}
	return false
}

func mcpValuesEqual(left, right any) bool {
	if wireNumberValid(left) && wireNumberValid(right) {
		return mcpNumberKey(left) == mcpNumberKey(right)
	}
	leftJSON, leftErr := json.Marshal(left)
	rightJSON, rightErr := json.Marshal(right)
	return leftErr == nil && rightErr == nil && bytes.Equal(leftJSON, rightJSON)
}

func mcpNumberKey(value any) string {
	var lexeme string
	switch number := value.(type) {
	case json.Number:
		lexeme = string(number)
	case float64:
		lexeme = fmt.Sprintf("%.17g", number)
	case int:
		lexeme = fmt.Sprint(number)
	case int64:
		lexeme = fmt.Sprint(number)
	case uint32:
		lexeme = fmt.Sprint(number)
	case uint64:
		lexeme = fmt.Sprint(number)
	default:
		return ""
	}
	parts := wireJSONNumberPattern.FindStringSubmatch(lexeme)
	if parts == nil {
		return lexeme
	}
	digits := strings.TrimLeft(parts[2]+parts[3], "0")
	if digits == "" {
		return "0"
	}
	exponent := new(big.Int)
	exponentText := parts[4]
	if exponentText == "" {
		exponentText = "0"
	}
	if _, ok := exponent.SetString(exponentText, 10); !ok {
		return lexeme
	}
	exponent.Sub(exponent, big.NewInt(int64(len(parts[3]))))
	for strings.HasSuffix(digits, "0") {
		digits = strings.TrimSuffix(digits, "0")
		exponent.Add(exponent, big.NewInt(1))
	}
	sign := ""
	if parts[1] == "-" {
		sign = "-"
	}
	return sign + digits + "e" + exponent.String()
}

func isMCPObject(value any) bool {
	_, ok := value.(map[string]any)
	return ok
}
