package clawossdk

import (
	"encoding/json"
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

func TestSelectedReadReplyAndClosedInputs(t *testing.T) {
	value := map[string]any{"version": json.Number("1.0"), "selection_id": "sr-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		"node": json.Number("7e0"), "access": "read", "state": "pending"}
	result, err := decodeSelectedRead(value)
	if err != nil || result.State != "pending" {
		t.Fatalf("pending is not selected: %v %v", result, err)
	}
	value["state"] = "selected"
	if result, err = decodeSelectedRead(value); err != nil || result.State != "selected" {
		t.Fatalf("selected decoder: %v %v", result, err)
	}
	value["owner_uid"] = 0
	if _, err := decodeSelectedRead(value); err == nil {
		t.Fatal("reply accepted owner claim")
	}
	if _, err := RequestFileRead(0); err == nil {
		t.Fatal("invalid node reached transport")
	}
	if _, err := FileReadStatus("foreign-window", 7); err == nil {
		t.Fatal("invalid selection reference reached transport")
	}
}
