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

const askClawLauncher = "/usr/local/bin/cos-ask-claw-launcher"
const askClawProtocol = 1
const askClawRequestLimit = 32 * 1024

// RequestFileRead proposes a native metadata inode; pending is not selected content.
func RequestFileRead(node uint64) (*GuiFileRead, error) {
	request := map[string]any{"node": node}
	if err := ValidateGuiFileReadRequest(request); err != nil {
		return nil, err
	}
	return selectedReadCall("request", request)
}

// FileReadStatus reads Root's status for this same window and proposed inode.
func FileReadStatus(selectionID string, node uint64) (*GuiFileRead, error) {
	request := map[string]any{"selection_id": selectionID, "node": node}
	if err := ValidateGuiFileReadStatusRequest(request); err != nil {
		return nil, err
	}
	return selectedReadCall("status", request)
}

func selectedReadCall(action string, request map[string]any) (*GuiFileRead, error) {
	value, err := selectedFileCall("__gui-file-read", action, request)
	if err != nil {
		return nil, err
	}
	return decodeSelectedRead(value)
}

// RequestFileWrite asks Root for independent protected write review, not read authority.
func RequestFileWrite(node uint64) (*GuiFileWrite, error) {
	request := map[string]any{"node": node}
	if err := ValidateGuiFileWriteRequest(request); err != nil {
		return nil, err
	}
	return selectedWriteCall("request", request)
}

// FileWriteStatus queries the same window and file's write selection.
func FileWriteStatus(selectionID string, node uint64) (*GuiFileWrite, error) {
	request := map[string]any{"selection_id": selectionID, "node": node}
	if err := ValidateGuiFileWriteStatusRequest(request); err != nil {
		return nil, err
	}
	return selectedWriteCall("status", request)
}

// ReserveFileSave reserves one private scratch name for the selected target only.
func ReserveFileSave(selectionID string, node uint64) (*GuiFileSave, error) {
	request := map[string]any{"selection_id": selectionID, "node": node}
	if err := ValidateGuiFileSaveReservationRequest(request); err != nil {
		return nil, err
	}
	value, err := selectedFileCall("__gui-file-write", "reserve", request)
	if err != nil {
		return nil, err
	}
	var result GuiFileSave
	if err := decodeSelectedFile(value, ValidateGuiFileSave, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func selectedWriteCall(action string, request map[string]any) (*GuiFileWrite, error) {
	value, err := selectedFileCall("__gui-file-write", action, request)
	if err != nil {
		return nil, err
	}
	var result GuiFileWrite
	if err := decodeSelectedFile(value, ValidateGuiFileWrite, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func selectedFileCall(bridge, action string, request map[string]any) (any, error) {
	input, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	response, err := cosCallJSON("GUI selected file",
		[]string{bridge, action, "--request-stdin"}, input)
	if err != nil {
		return nil, err
	}
	return response.Data, nil
}

// RequestFileCreate proposes one absent literal child, never a directory grant.
func RequestFileCreate(parentNode uint64, name string) (*GuiFileCreate, error) {
	request := map[string]any{"parent_node": parentNode, "name": name}
	if err := ValidateGuiFileCreateRequest(request); err != nil { return nil, err }
	return selectedCreateCall("request", request)
}

// FileCreateStatus distinguishes committed creation from selection/reservation.
func FileCreateStatus(selectionID string, parentNode uint64) (*GuiFileCreate, error) {
	request := map[string]any{"selection_id": selectionID, "parent_node": parentNode}
	if err := ValidateGuiFileCreateStatusRequest(request); err != nil { return nil, err }
	return selectedCreateCall("status", request)
}

// ReserveFileCreate reserves private scratch for one atomic NOREPLACE creation.
func ReserveFileCreate(selectionID string, parentNode uint64) (*GuiFileCreateSlot, error) {
	request := map[string]any{"selection_id": selectionID, "parent_node": parentNode}
	if err := ValidateGuiFileCreateReservationRequest(request); err != nil { return nil, err }
	value, err := selectedFileCall("__gui-file-create", "reserve", request)
	if err != nil { return nil, err }
	var result GuiFileCreateSlot
	if err := decodeSelectedFile(value, ValidateGuiFileCreateSlot, &result); err != nil { return nil, err }
	return &result, nil
}

func selectedCreateCall(action string, request map[string]any) (*GuiFileCreate, error) {
	value, err := selectedFileCall("__gui-file-create", action, request)
	if err != nil { return nil, err }
	var result GuiFileCreate
	if err := decodeSelectedFile(value, ValidateGuiFileCreate, &result); err != nil { return nil, err }
	return &result, nil
}

func decodeSelectedRead(value any) (*GuiFileRead, error) {
	var result GuiFileRead
	if err := decodeSelectedFile(value, ValidateGuiFileRead, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func decodeSelectedFile(value any, validate func(any) error, result any) error {
	if err := validate(value); err != nil {
		return err
	}
	normalized := make(map[string]any)
	for key, field := range value.(map[string]any) {
		normalized[key] = field
	}
	for _, key := range []string{"version", "node", "parent_node", "expires_in_ms"} {
		if field, ok := normalized[key]; ok {
			normalized[key] = asUint64(field)
		}
	}
	data, err := json.Marshal(normalized)
	if err != nil {
		return err
	}
	return json.Unmarshal(data, result)
}

// IsGUILaunch reports whether the bridge marked this process as an
// authenticated desktop launch.
func IsGUILaunch() bool {
	return os.Getenv("COS_APP_GUI") == "1"
}

// GuiContext is the kernel context handed to a desktop app at launch.
type GuiContext struct {
	// AppID is the kernel-assigned app identity (COS_APP_ID).
	AppID string
	// Files are paths the launcher passed (%F), decoded from
	// COS_ARGS_JSON. Empty when launched without file arguments.
	Files []string
}

// Context builds the GuiContext for the current GUI launch. It rejects
// missing App identity and malformed host arguments. Files defaults to
// the launcher's COS_ARGS_JSON array when nil.
func Context(files []string) (*GuiContext, error) {
	appID := os.Getenv("COS_APP_ID")
	if appID == "" {
		return nil, fmt.Errorf("COS_APP_ID is required for a GUI launch")
	}
	if files == nil {
		var err error
		files, err = filesFromEnv()
		if err != nil {
			return nil, err
		}
	}
	return &GuiContext{AppID: appID, Files: files}, nil
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

func filesFromEnv() ([]string, error) {
	raw := os.Getenv("COS_ARGS_JSON")
	if raw == "" {
		return nil, nil
	}
	var parsed []string
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("COS_ARGS_JSON must be an array of strings: %w", err)
	}
	return parsed, nil
}
