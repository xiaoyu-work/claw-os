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
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"strings"
	"syscall"
	"time"
)

// GUICommand is the command value the bridge passes (and the default
// `desktop.exec`) when an app is launched as a GUI.
const GUICommand = "--gui"
const askClawLauncher = "/usr/local/bin/cos-ask-claw-launcher"
const askClawProtocol = 1
const askClawRequestLimit = 32 * 1024

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
// fixed packaged launcher's authenticated Unix-socket protocol. Pass a
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
	cmd := exec.Command(askClawLauncher, "--protocol", fmt.Sprint(askClawProtocol))
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	cmd.Stderr = nil
	if err := cmd.Start(); err != nil {
		return err
	}
	type announcementResult struct {
		endpoint string
		err      error
	}
	announced := make(chan announcementResult, 1)
	go func() {
		line, err := bufio.NewReader(io.LimitReader(stdout, 257)).ReadString('\n')
		if err != nil {
			announced <- announcementResult{err: err}
			return
		}
		if len(line) > 256 {
			announced <- announcementResult{err: fmt.Errorf("Ask Claw socket announcement is too long")}
			return
		}
		prefix := fmt.Sprintf("SOCKET %d @", askClawProtocol)
		if !strings.HasPrefix(line, prefix) || !strings.HasSuffix(line, "\n") {
			announced <- announcementResult{err: fmt.Errorf("invalid Ask Claw socket announcement")}
			return
		}
		announced <- announcementResult{
			endpoint: "@" + strings.TrimSuffix(strings.TrimPrefix(line, prefix), "\n"),
		}
	}()
	var endpoint string
	select {
	case result := <-announced:
		if result.err != nil {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
			return result.err
		}
		endpoint = result.endpoint
	case <-time.After(5 * time.Second):
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return fmt.Errorf("Ask Claw launcher readiness timed out")
	}
	if endpoint == "@" {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return fmt.Errorf("empty Ask Claw socket endpoint")
	}
	connection, err := net.DialTimeout("unix", endpoint, 5*time.Second)
	if err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	defer connection.Close()
	if err := connection.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	readyMessage := make([]byte, len("READY 1\n"))
	if _, err := io.ReadFull(connection, readyMessage); err != nil || string(readyMessage) != "READY 1\n" {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		if err != nil {
			return err
		}
		return fmt.Errorf("unexpected Ask Claw launcher handshake")
	}
	request := map[string]any{"protocol": askClawProtocol, "app": c.AppID, "hint": hint}
	payload, err := json.Marshal(request)
	if err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	if len(payload) > askClawRequestLimit {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return fmt.Errorf("Ask Claw request exceeds the protocol limit")
	}
	frame := make([]byte, 4+len(payload))
	binary.BigEndian.PutUint32(frame, uint32(len(payload)))
	copy(frame[4:], payload)
	if _, err := io.Copy(connection, bytes.NewReader(frame)); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	unixConnection, ok := connection.(*net.UnixConn)
	if !ok {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return fmt.Errorf("Ask Claw launcher did not provide a Unix socket")
	}
	if err := unixConnection.CloseWrite(); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	accepted := make([]byte, len("ACCEPTED 1\n"))
	if _, err := io.ReadFull(connection, accepted); err != nil || string(accepted) != "ACCEPTED 1\n" {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		if err != nil {
			return err
		}
		return fmt.Errorf("unexpected Ask Claw acceptance response")
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
