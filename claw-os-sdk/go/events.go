package clawossdk

import "encoding/json"

// PublishEvent reports data from an authenticated App session, never execution authority.
func PublishEvent(eventType, key string, payload map[string]any, entityID string) (map[string]any, error) {
	request := map[string]any{"event_type": eventType, "key": key}
	if payload != nil {
		request["payload"] = payload
	}
	if entityID != "" {
		request["entity_id"] = entityID
	}
	input, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	if len(input) > 20*1024 {
		return nil, &UnavailableError{Msg: "App event request exceeds 20 KiB"}
	}
	outcome, err := cosCallJSON("App event publication", []string{"__app_event", "publish"}, input)
	if err != nil {
		return nil, err
	}
	value, ok := outcome.Data.(map[string]any)
	if !ok {
		return nil, &UnavailableError{Msg: "App event reply is not an object"}
	}
	return value, nil
}
