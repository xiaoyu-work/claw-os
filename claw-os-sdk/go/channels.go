package clawossdk

import "encoding/json"

// ControlChannel derives the App/owner in Root; it cannot configure senders or delegate tasks.
func ControlChannel(action, eventID, text string, confirm *bool) (map[string]any, error) {
	request := map[string]any{"action": action}
	if eventID != "" {
		request["event_id"] = eventID
	}
	if text != "" {
		request["text"] = text
	}
	if confirm != nil {
		request["confirm"] = *confirm
	}
	input, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	if len(input) > 20*1024 {
		return nil, &UnavailableError{Msg: "channel control exceeds 20 KiB"}
	}
	outcome, err := cosCallJSON("channel control", []string{"__channel", "control"}, input)
	if err != nil {
		return nil, err
	}
	value, ok := outcome.Data.(map[string]any)
	if !ok {
		return nil, &UnavailableError{Msg: "channel reply is not an object"}
	}
	return value, nil
}
