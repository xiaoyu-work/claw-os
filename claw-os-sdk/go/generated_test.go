package clawossdk

import (
	"encoding/json"
	"strings"
	"testing"
)

func decodeWireTest(t *testing.T, body string) map[string]any {
	t.Helper()
	var value map[string]any
	decoder := json.NewDecoder(strings.NewReader(body))
	decoder.UseNumber()
	if err := decoder.Decode(&value); err != nil {
		t.Fatal(err)
	}
	return value
}

func validAIWire(t *testing.T) map[string]any {
	return decodeWireTest(t, `{
		"text":"hello","model":"m","provider":"p","verb":"ai.chat",
		"usage":{"input_tokens":1,"output_tokens":2,"units":3},
		"budget":{"period":"2026-08","units_used":3,"units_cap":100},
		"review":{"safety":"strict","prompt_redacted":false},
		"tool_calls":[{"id":"c1","name":"echo","input":{"value":"ok"}}]
	}`)
}

func TestAIValidatorEnforcesSharedContract(t *testing.T) {
	cases := []struct {
		mutate func(map[string]any)
		code   string
		path   string
	}{
		{func(value map[string]any) { delete(value, "text") }, WireRequired, "$.text"},
		{func(value map[string]any) { value["usage"].(map[string]any)["input_tokens"] = "1" }, WireType, "$.usage.input_tokens"},
		{func(value map[string]any) { value["usage"].(map[string]any)["units"] = json.Number("-1") }, WireMinimum, "$.usage.units"},
		{func(value map[string]any) { value["verb"] = "ai.unknown" }, WireEnum, "$.verb"},
		{func(value map[string]any) { value["usage"].(map[string]any)["extra"] = true }, WireUnknownField, "$.usage.extra"},
		{func(value map[string]any) { delete(value["tool_calls"].([]any)[0].(map[string]any), "name") }, WireRequired, "$.tool_calls[0].name"},
		{func(value map[string]any) {
			value["tool_calls"].([]any)[0].(map[string]any)["input"] = "scalar"
		}, WireType, "$.tool_calls[0].input"},
	}
	for _, test := range cases {
		value := validAIWire(t)
		test.mutate(value)
		err := ValidateAi(value)
		wireErr, ok := err.(*WireDecodeError)
		if !ok || wireErr.Code != test.code || wireErr.Path != test.path {
			t.Fatalf("ValidateAi error = %#v, want code %s path %s", err, test.code, test.path)
		}
	}
}

func TestStructuredItemsAreValidatedWithoutSkipping(t *testing.T) {
	if err := ValidateAi(validAIWire(t)); err != nil {
		t.Fatal(err)
	}
	if err := ValidateTool(decodeWireTest(t, `{"tool":"echo","app_id":"app","status":"ok","result":null}`)); err != nil {
		t.Fatal(err)
	}
	catalog := decodeWireTest(t, `{"tools":[
		{"name":"echo","summary":"Echo","verb":"ipc.invoke","stability":"stable","args_schema":{},"returns_schema":{}},
		7
	]}`)
	err := ValidateToolCatalog(catalog)
	wireErr, ok := err.(*WireDecodeError)
	if !ok || wireErr.Code != WireType || wireErr.Path != "$.tools[1]" {
		t.Fatalf("ValidateToolCatalog error = %#v", err)
	}
}
