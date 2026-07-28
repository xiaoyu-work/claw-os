// Subprocess transport shared by every Go SDK module.
//
// Like the Python, Rust, and Node SDKs, the Go SDK is a thin client
// over wire protocol v1: it shells out to the `cos` binary, which reads
// a request from argv and writes a JSON envelope to stdout. The
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
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// DefaultTimeout bounds every shell-out to `cos` so a wedged child
// never blocks the calling app forever.
const DefaultTimeout = 60 * time.Second

// maxOutput caps captured stdout+stderr (8 MiB) — generous for
// embeddings / base64 artifact paths without unbounded RAM.
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

// CosBinary resolves the `cos` binary. It honors CLAW_COS_BIN then
// COS_BIN (both used across the SDK family and dev/test setups),
// falling back to `cos` on $PATH.
func CosBinary() string {
	if b := os.Getenv("CLAW_COS_BIN"); b != "" {
		return b
	}
	if b := os.Getenv("COS_BIN"); b != "" {
		return b
	}
	return "cos"
}

// cosOutcome is the parsed result of one `cos` invocation.
type cosOutcome struct {
	// Envelope is the top-level JSON object the kernel emitted.
	Envelope map[string]any
	// Status is the process exit code (so callers can treat a non-zero
	// exit as failure even when stdout was valid JSON).
	Status int
}

// cosCallJSON runs `cos <args>` and parses its stdout as a JSON object.
// label names the logical call (e.g. "cos ai chat") for error messages.
// Returns an *UnavailableError for transport problems; the envelope is
// returned untouched so domain code decides what a non-zero Status or an
// "error" field means.
func cosCallJSON(label string, args []string) (*cosOutcome, error) {
	bin := CosBinary()
	ctx, cancel := context.WithTimeout(context.Background(), DefaultTimeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, bin, args...)
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
		text = strings.TrimSpace(stderr.String())
	}
	if text == "" {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned no output (exit %d)", label, status)}
	}
	if len(text) > maxOutput {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s output exceeded %d bytes", label, maxOutput)}
	}

	var env map[string]any
	decoder := json.NewDecoder(strings.NewReader(text))
	decoder.UseNumber()
	if err := decoder.Decode(&env); err != nil {
		return nil, &UnavailableError{Msg: fmt.Sprintf("%s returned non-JSON output: %s", label, truncate(text, 200))}
	}
	return &cosOutcome{Envelope: env, Status: status}, nil
}

func (o *cosOutcome) hasError() bool {
	_, ok := o.Envelope["error"]
	return ok
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

func asFloat(v any) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case int:
		return float64(n)
	case json.Number:
		value, _ := n.Float64()
		return value
	}
	return 0
}

func asInt(v any) int64 { return int64(asFloat(v)) }

func asUint64(v any) uint64 {
	switch n := v.(type) {
	case json.Number:
		value, _ := strconv.ParseUint(string(n), 10, 64)
		return value
	case uint64:
		return n
	case uint32:
		return uint64(n)
	case int64:
		if n >= 0 {
			return uint64(n)
		}
	case int:
		if n >= 0 {
			return uint64(n)
		}
	case float64:
		if n >= 0 {
			return uint64(n)
		}
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
