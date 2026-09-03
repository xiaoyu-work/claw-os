package clawossdk

import (
	"os"
	"testing"
)

func TestIsGUILaunch(t *testing.T) {
	os.Unsetenv("COS_APP_GUI")
	if IsGUILaunch() {
		t.Fatal("missing host context should not be a GUI launch")
	}
	t.Setenv("COS_APP_GUI", "1")
	if !IsGUILaunch() {
		t.Fatal("COS_APP_GUI=1 should be a GUI launch")
	}
	t.Setenv("COS_APP_GUI", "0")
	if IsGUILaunch() {
		t.Fatal("COS_APP_GUI=0 should not be a GUI launch")
	}
}

func TestContextReadsEnv(t *testing.T) {
	t.Setenv("COS_APP_ID", "notes")
	t.Setenv("COS_ARGS_JSON", `["/a.md", "/b.md"]`)
	ctx, err := Context(nil)
	if err != nil {
		t.Fatal(err)
	}
	if ctx.AppID != "notes" {
		t.Fatalf("AppID = %q", ctx.AppID)
	}
	if len(ctx.Files) != 2 || ctx.Files[0] != "/a.md" || ctx.Files[1] != "/b.md" {
		t.Fatalf("Files = %v", ctx.Files)
	}
}

func TestContextRequiresIdentity(t *testing.T) {
	os.Unsetenv("COS_APP_ID")
	os.Unsetenv("COS_ARGS_JSON")
	if _, err := Context([]string{"/explicit"}); err == nil {
		t.Fatal("missing COS_APP_ID should fail")
	}
}

func TestContextBadArgsJSON(t *testing.T) {
	t.Setenv("COS_APP_ID", "notes")
	t.Setenv("COS_ARGS_JSON", "not json")
	if _, err := Context(nil); err == nil {
		t.Fatal("bad COS_ARGS_JSON should fail")
	}
}

func TestOpenAgentOverlayMissingBinary(t *testing.T) {
	t.Setenv("COS_AGENT_UI_BIN", "/nonexistent/attacker")
	ctx := &GuiContext{AppID: "notes"}
	if err := ctx.OpenAgentOverlay(""); err == nil {
		t.Fatal("expected error when overlay binary is missing")
	}
}
