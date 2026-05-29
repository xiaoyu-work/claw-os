package clawossdk

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// fakeCos writes an executable that prints stdout and exits with
// exitCode, recording the argv it received (one per line) to a sidecar
// file. It returns the binary path and the argv-record path.
func fakeCos(t *testing.T, stdout string, exitCode int) (bin, argvOut string) {
	t.Helper()
	dir := t.TempDir()
	bin = filepath.Join(dir, "cos")
	argvOut = filepath.Join(dir, "argv.txt")
	payload := filepath.Join(dir, "stdout.json")
	if err := os.WriteFile(payload, []byte(stdout), 0o644); err != nil {
		t.Fatalf("write payload: %v", err)
	}
	script := "#!/bin/sh\n" +
		"printf '%s\\n' \"$@\" > " + argvOut + "\n" +
		"cat " + payload + "\n" +
		"exit " + itoa(exitCode) + "\n"
	if err := os.WriteFile(bin, []byte(script), 0o755); err != nil {
		t.Fatalf("write fake cos: %v", err)
	}
	return bin, argvOut
}

func itoa(i int) string {
	if i == 0 {
		return "0"
	}
	neg := i < 0
	if neg {
		i = -i
	}
	var b []byte
	for i > 0 {
		b = append([]byte{byte('0' + i%10)}, b...)
		i /= 10
	}
	if neg {
		b = append([]byte{'-'}, b...)
	}
	return string(b)
}

// withCos points CLAW_COS_BIN at the fake and runs fn, restoring env.
func withCos(t *testing.T, bin string, env map[string]string, fn func()) {
	t.Helper()
	t.Setenv("CLAW_COS_BIN", bin)
	os.Unsetenv("COS_BIN")
	for k, v := range env {
		if v == "" {
			os.Unsetenv(k)
		} else {
			t.Setenv(k, v)
		}
	}
	fn()
}

func readArgv(t *testing.T, path string) []string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read argv: %v", err)
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) == 1 && lines[0] == "" {
		return nil
	}
	return lines
}

func TestCosBinaryResolution(t *testing.T) {
	t.Setenv("CLAW_COS_BIN", "/x/claw")
	if got := CosBinary(); got != "/x/claw" {
		t.Fatalf("CLAW_COS_BIN should win, got %q", got)
	}
	os.Unsetenv("CLAW_COS_BIN")
	t.Setenv("COS_BIN", "/x/cosbin")
	if got := CosBinary(); got != "/x/cosbin" {
		t.Fatalf("COS_BIN fallback failed, got %q", got)
	}
	os.Unsetenv("COS_BIN")
	if got := CosBinary(); got != "cos" {
		t.Fatalf("default should be cos, got %q", got)
	}
}

func TestCosCallJSONNonJSON(t *testing.T) {
	bin, _ := fakeCos(t, "not json at all", 0)
	withCos(t, bin, nil, func() {
		_, err := cosCallJSON("test", []string{"x"})
		if _, ok := err.(*UnavailableError); !ok {
			t.Fatalf("expected UnavailableError, got %T %v", err, err)
		}
	})
}

func TestCosCallJSONMissingBinary(t *testing.T) {
	withCos(t, "/nonexistent/cos-xyz", nil, func() {
		_, err := cosCallJSON("test", []string{"x"})
		if _, ok := err.(*UnavailableError); !ok {
			t.Fatalf("expected UnavailableError for missing binary, got %T %v", err, err)
		}
	})
}
