package clawossdk

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"
)

func decodeWireValue(t *testing.T, body string) any {
	t.Helper()
	var value any
	decoder := json.NewDecoder(strings.NewReader(body))
	decoder.UseNumber()
	if err := decoder.Decode(&value); err != nil {
		t.Fatal(err)
	}
	return value
}

func decodeWireTest(t *testing.T, body string) map[string]any {
	t.Helper()
	return decodeWireValue(t, body).(map[string]any)
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

func TestIntegerValidationUsesJSONSchemaMathematicalSemantics(t *testing.T) {
	accepted := map[string]uint64{
		"1.0":                  1,
		"1e0":                  1,
		"1.5e1":                15,
		"9007199254740992":     9007199254740992,
		"18446744073709551615": ^uint64(0),
	}
	for literal, expected := range accepted {
		value := validAIWire(t)
		value["usage"].(map[string]any)["units"] = json.Number(literal)
		if err := ValidateAi(value); err != nil {
			t.Fatalf("%s: %v", literal, err)
		}
		response, err := parseResponse(value)
		if err != nil || response.Usage.Units != expected {
			t.Fatalf("%s: response=%+v error=%v", literal, response, err)
		}
	}

	for _, literal := range []string{"1.5", "15e-1", "1e-400", "9007199254740990.5"} {
		value := validAIWire(t)
		value["usage"].(map[string]any)["units"] = json.Number(literal)
		err := ValidateAi(value).(*WireDecodeError)
		if err.Code != WireType || err.Path != "$.usage.units" {
			t.Fatalf("%s: fractional error = %#v", literal, err)
		}
	}

	value := validAIWire(t)
	value["usage"].(map[string]any)["units"] = json.Number("18446744073709551616")
	if err := ValidateAi(value).(*WireDecodeError); err.Code != WireMaximum {
		t.Fatalf("oversized error = %#v", err)
	}

	value = validAIWire(t)
	value["usage"].(map[string]any)["units"] = json.Number("18446744073709551615.5")
	if err := ValidateAi(value).(*WireDecodeError); err.Code != WireType {
		t.Fatalf("fractional-above-max error = %#v", err)
	}
}

func TestV1ToolInputsRemainUnrestricted(t *testing.T) {
	for _, input := range []any{"scalar", []any{json.Number("1"), true}, nil} {
		value := validAIWire(t)
		value["tool_calls"].([]any)[0].(map[string]any)["input"] = input
		if err := ValidateAi(value); err != nil {
			t.Fatalf("input %#v: %v", input, err)
		}
		response, err := parseResponse(value)
		if err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(response.ToolCalls[0].Input, input) {
			t.Fatalf("input = %#v, want %#v", response.ToolCalls[0].Input, input)
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

func TestRootTypeAndBudgetShowContract(t *testing.T) {
	root := decodeWireValue(t, "null")
	for _, validator := range []func(any) error{
		ValidateAi,
		ValidateTool,
		ValidateToolCatalog,
	} {
		err := validator(root).(*WireDecodeError)
		if err.Code != WireType || err.Path != "$" {
			t.Fatalf("root error = %#v", err)
		}
	}
	if err := ValidateBudgetShow(decodeWireValue(
		t,
		`{"app":"notes","period":"2026-08","units_used":7}`,
	)); err != nil {
		t.Fatal(err)
	}
}
