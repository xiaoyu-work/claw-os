// Subprocess transport shared by every Go SDK module.
//
// Like the Python, Rust, and Node SDKs, the Go SDK is a thin client
// over wire protocol v1: it shells out to the `cos` binary, which reads
// non-sensitive routing flags from argv and writes a JSON envelope to stdout.
// AI prompt bodies are passed through private temporary files. The
// subprocess model is intentional — identity, audit, and session
// context are inherited from process ancestry (kernel-spawned parent →
// app process → cos child). A pure in-process binding could not prove
// "App X is making this call".

package clawossdk

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"
)

// DefaultTimeout bounds every shell-out to `cos` so a wedged child
// never blocks the calling app forever.
const DefaultTimeout = 60 * time.Second

// maxOutput caps captured stdout+stderr (8 MiB) — generous for
// large structured tool results without unbounded RAM.
const maxOutput = 8 * 1024 * 1024

// UnavailableError means the `cos` binary could not be invoked, timed
// out, or returned something that was not a JSON envelope.
type UnavailableError struct{ Msg string }

func (e *UnavailableError) Error() string { return e.Msg }

// DeniedError means a gate (capability / origin / budget / unknown verb
// / arg shape) refused the call. Payload holds the structured error
// envelope the kernel returned, suitable for forwarding to the agent.
type DeniedError struct{ Payload map[string]any }

func (e *DeniedError) Error() string {
	if msg, ok := e.Payload["error"].(string); ok && msg != "" {
		return msg
	}
	return "call denied"
}

// CosBinary resolves the `cos` binary. It honors CLAW_COS_BIN,
// falling back to `cos` on $PATH.
func CosBinary() string {
	if b := os.Getenv("CLAW_COS_BIN"); b != "" {
		return b
	}
	return "cos"
}

// cosOutcome is the parsed result of one `cos` invocation.
type cosOutcome struct {
	Data any
}

// cosCallJSON runs `cos <args>` and parses its stdout as a JSON object.
// label names the logical call (e.g. "cos ai chat") for error messages.
// Returns an *UnavailableError for transport/protocol problems and a
// *DeniedError for a valid kernel error envelope.
func cosCallJSON(label string, args []string) (*cosOutcome, error) {
	bin := CosBinary()
	ctx, cancel := context.WithTimeout(context.Background(), DefaultTimeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, bin, append([]string{"--wire=1"}, args...)...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	status := 0
	runErr := cmd.Run()
	if runErr != nil {
		if ctx.Err() == context.DeadlineExceeded {
			return nil, &UnavailableError{Msg: fmt.Sprintf("%s timed out after %s", label, DefaultTimeout)}
		}
		var exitErr *exec.ExitError
		if errors.As(runErr, &exitErr) {
			status = exitErr.ExitCode()
		} else {
			return nil, &UnavailableError{Msg: fmt.Sprintf(
				"could not spawn %s: %v (set CLAW_COS_BIN or install cos)", bin, runErr)}
		}
	}

	text := strings.TrimSpace(stdout.String())
	if text == "" {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned no wire response (exit %d)", label, status)}
	}
	if len(text) > maxOutput {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s output exceeded %d bytes", label, maxOutput)}
	}

	var env any
	decoder := json.NewDecoder(strings.NewReader(text))
	decoder.UseNumber()
	if err := decoder.Decode(&env); err != nil {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned non-JSON output: %s", label, truncate(text, 200))}
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned trailing JSON data", label)}
	}
	if err := ValidateEnvelope(env); err != nil {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned an invalid wire envelope: %v", label, err)}
	}
	envelope := asMap(env)
	if asBool(envelope["ok"]) {
		if status != 0 {
			return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned a success envelope with exit %d", label, status)}
		}
		return &cosOutcome{Data: envelope["data"]}, nil
	}
	if status == 0 {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned an error envelope with exit 0", label)}
	}
	return nil, &DeniedError{Payload: envelope}
}

func truncate(s string, limit int) string {
	if len(s) <= limit {
		return s
	}
	return s[:limit] + fmt.Sprintf("... [%d more bytes elided]", len(s)-limit)
}

// --- small JSON coercion helpers shared by ai.go / tools.go ----------

func asString(v any) string {
	if s, ok := v.(string); ok {
		return s
	}
	return ""
}

func asUint64(v any) uint64 {
	number, ok := wireExactInteger(v)
	if ok && number.overflow == 0 && number.value.Sign() >= 0 && number.value.IsUint64() {
		return number.value.Uint64()
	}
	return 0
}

func asUint32(v any) uint32 {
	value := asUint64(v)
	if value > uint64(^uint32(0)) {
		return 0
	}
	return uint32(value)
}

func asBool(v any) bool {
	b, _ := v.(bool)
	return b
}

func asMap(v any) map[string]any {
	if m, ok := v.(map[string]any); ok {
		return m
	}
	return map[string]any{}
}
