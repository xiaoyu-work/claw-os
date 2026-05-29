package clawossdk

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestIsGUILaunch(t *testing.T) {
	os.Unsetenv("COS_APP_GUI")
	if IsGUILaunch("") {
		t.Fatal("no env, no command should not be a GUI launch")
	}
	if !IsGUILaunch("--gui") {
		t.Fatal("--gui command should be a GUI launch")
	}
	t.Setenv("COS_APP_GUI", "1")
	if !IsGUILaunch("") {
		t.Fatal("COS_APP_GUI=1 should be a GUI launch")
	}
	t.Setenv("COS_APP_GUI", "0")
	if IsGUILaunch("") {
		t.Fatal("COS_APP_GUI=0 should not be a GUI launch")
	}
}

func TestContextReadsEnv(t *testing.T) {
	t.Setenv("COS_APP_ID", "notes")
	t.Setenv("COS_ARGS_JSON", `["/a.md", "/b.md"]`)
	ctx := Context(nil)
	if ctx.AppID != "notes" {
		t.Fatalf("AppID = %q", ctx.AppID)
	}
	if len(ctx.Files) != 2 || ctx.Files[0] != "/a.md" || ctx.Files[1] != "/b.md" {
		t.Fatalf("Files = %v", ctx.Files)
	}
}

func TestContextDefaultsAndExplicitFiles(t *testing.T) {
	os.Unsetenv("COS_APP_ID")
	os.Unsetenv("COS_ARGS_JSON")
	ctx := Context([]string{"/explicit"})
	if ctx.AppID != "unknown" {
		t.Fatalf("AppID should default to unknown, got %q", ctx.AppID)
	}
	if len(ctx.Files) != 1 || ctx.Files[0] != "/explicit" {
		t.Fatalf("explicit files should win, got %v", ctx.Files)
	}
}

func TestContextBadArgsJSON(t *testing.T) {
	t.Setenv("COS_APP_ID", "notes")
	t.Setenv("COS_ARGS_JSON", "not json")
	ctx := Context(nil)
	if ctx.Files != nil {
		t.Fatalf("bad COS_ARGS_JSON should yield no files, got %v", ctx.Files)
	}
}

func TestOpenAgentOverlaySpawns(t *testing.T) {
	dir := t.TempDir()
	marker := filepath.Join(dir, "spawned")
	bin := filepath.Join(dir, "cos-agent-ui")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + marker + "\n"
	if err := os.WriteFile(bin, []byte(script), 0o755); err != nil {
		t.Fatalf("write fake overlay: %v", err)
	}
	t.Setenv("COS_AGENT_UI_BIN", bin)

	ctx := Context([]string{})
	if err := ctx.OpenAgentOverlay("current doc"); err != nil {
		t.Fatalf("OpenAgentOverlay error: %v", err)
	}
	// Detached child: poll briefly for the marker.
	var data []byte
	for i := 0; i < 50; i++ {
		if b, err := os.ReadFile(marker); err == nil {
			data = b
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if len(data) == 0 {
		t.Fatal("overlay was not spawned (no marker)")
	}
}

func TestOpenAgentOverlayMissingBinary(t *testing.T) {
	t.Setenv("COS_AGENT_UI_BIN", "/nonexistent/cos-agent-ui-xyz")
	ctx := Context(nil)
	if err := ctx.OpenAgentOverlay(""); err == nil {
		t.Fatal("expected error when overlay binary is missing")
	}
}
