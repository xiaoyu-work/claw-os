// Desktop GUI bootstrap for Claw OS Go apps.
//
// This is the GUI counterpart to the stable AI Chat and tool
// (Tool/Catalog) helpers. It does NOT wrap a UI toolkit: a Claw OS
// desktop app draws its own window in whatever toolkit it likes ("World
// A"). This file only hands the app the small amount of kernel context
// it receives once kernel-spawned as a GUI, plus the one privileged
// action a GUI commonly wants — summoning the agent overlay.
//
// When an app declares a `desktop` block in app.json, `cos app install`
// writes a launcher whose Exec is `cos app <id> --gui`. Activating it
// routes the launch back through the kernel, which spawns the app with
// COS_APP_GUI=1 and COS_APP_ID set, so identity/audit/consent apply to
// the GUI exactly as to the headless path.

package clawossdk

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"syscall"
	"time"
)

// GUICommand is the command value the bridge passes (and the default
// `desktop.exec`) when an app is launched as a GUI.
const GUICommand = "--gui"
const askClawLauncher = "/usr/local/bin/cos-ask-claw-launcher"

// IsGUILaunch reports whether the current invocation is a desktop GUI
// launch. It prefers the COS_APP_GUI environment variable the bridge
// sets for the long-lived GUI process; as a fallback (so apps with a
// custom desktop.exec still work) a command equal to GUICommand is also
// treated as a GUI launch. Pass "" when you have no command value.
func IsGUILaunch(command string) bool {
	if os.Getenv("COS_APP_GUI") == "1" {
		return true
	}
	return command == GUICommand
}

// GuiContext is the kernel context handed to a desktop app at launch.
type GuiContext struct {
	// AppID is the kernel-assigned app identity (COS_APP_ID).
	AppID string
	// Files are paths the launcher passed (%F), decoded from
	// COS_ARGS_JSON. Empty when launched without file arguments.
	Files []string
}

// Context builds the GuiContext for the current GUI launch. AppID is
// read from COS_APP_ID (set by the kernel when it spawns the GUI).
// files defaults to the launcher's file arguments (decoded from
// COS_ARGS_JSON) when nil.
func Context(files []string) *GuiContext {
	appID := os.Getenv("COS_APP_ID")
	if appID == "" {
		appID = "unknown"
	}
	if files == nil {
		files = filesFromEnv()
	}
	return &GuiContext{AppID: appID, Files: files}
}

// OpenAgentOverlay summons the system "Ask Claw" agent overlay through the
// fixed packaged launcher's versioned READY/stdin protocol. Pass a
// non-empty hint to ground the agent's first response in the app's
// current state without polluting the visible chat transcript.
//
// The overlay is detached: it outlives this call and is not tied to the
// app's stdio or event loop. Returns an error if the overlay binary is
// missing (e.g. a headless box with no desktop shell).
func (c *GuiContext) OpenAgentOverlay(hint string) error {
	if err := validateAskClawLauncher(); err != nil {
		return err
	}
	cmd := exec.Command(askClawLauncher, "--protocol", "1")
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	cmd.Stderr = nil
	if err := cmd.Start(); err != nil {
		return err
	}
	ready := make(chan error, 1)
	go func() {
		line, err := bufio.NewReader(stdout).ReadString('\n')
		if err == nil && line != "READY 1\n" {
			err = fmt.Errorf("unexpected Ask Claw launcher handshake")
		}
		ready <- err
	}()
	select {
	case err := <-ready:
		if err != nil {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
			return err
		}
	case <-time.After(5 * time.Second):
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return fmt.Errorf("Ask Claw launcher readiness timed out")
	}
	request := map[string]any{"protocol": 1, "app": c.AppID, "hint": hint}
	if err := json.NewEncoder(stdin).Encode(request); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	if err := stdin.Close(); err != nil {
		return err
	}
	go func() { _ = cmd.Wait() }()
	return nil
}

func validateAskClawLauncher() error {
	for _, path := range []string{"/usr", "/usr/local", "/usr/local/bin"} {
		info, err := os.Lstat(path)
		if err != nil {
			return err
		}
		statInfo, ok := info.Sys().(*syscall.Stat_t)
		if !ok || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() ||
			statInfo.Uid != 0 || info.Mode().Perm()&0o022 != 0 {
			return fmt.Errorf("untrusted Ask Claw launcher parent: %s", path)
		}
	}
	info, err := os.Lstat(askClawLauncher)
	if err != nil {
		return err
	}
	statInfo, ok := info.Sys().(*syscall.Stat_t)
	if !ok || info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() ||
		statInfo.Uid != 0 || info.Mode().Perm()&0o111 == 0 ||
		info.Mode().Perm()&0o022 != 0 {
		return fmt.Errorf("untrusted Ask Claw launcher")
	}
	return nil
}

func filesFromEnv() []string {
	raw := os.Getenv("COS_ARGS_JSON")
	if raw == "" {
		return nil
	}
	var parsed []string
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil
	}
	return parsed
}
