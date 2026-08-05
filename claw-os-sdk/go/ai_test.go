package clawossdk

import (
	"slices"
	"testing"
)

const okChat = `{
  "verb": "ai.chat",
  "text": "hello there",
  "model": "m",
  "provider": "p",
  "usage": {"input_tokens": 3, "output_tokens": 5, "units": 8},
  "budget": {"period": "2026-05", "units_used": 8, "units_cap": 1000},
  "review": {"safety": "strict", "prompt_redacted": false},
  "tool_calls": [{"id": "c1", "name": "fs.read_text", "input": {"path": "/x"}}]
}`

func TestChatBuildsArgvAndParses(t *testing.T) {
	bin, argvOut := fakeCos(t, okChat, 0)
	var res *AiResponse
	var err error
	withCos(t, bin, map[string]string{"COS_APP_ID": "notes"}, func() {
		res, err = Chat("summarise", ChatOptions{
			Origin:   "external-content",
			MaxUnits: 100,
			Tools:    []string{"fs.read_text"},
		})
	})
	if err != nil {
		t.Fatalf("Chat returned error: %v", err)
	}
	if res.Text != "hello there" {
		t.Fatalf("text = %q", res.Text)
	}
	if res.Usage.Units != 8 {
		t.Fatalf("units = %v", res.Usage.Units)
	}
	if res.Budget.UnitsCap != 1000 {
		t.Fatalf("units_cap = %v", res.Budget.UnitsCap)
	}
	if len(res.ToolCalls) != 1 || res.ToolCalls[0].Name != "fs.read_text" {
		t.Fatalf("tool_calls = %+v", res.ToolCalls)
	}

	argv := readArgv(t, argvOut)
	want := []string{"ai", "chat", "--app", "notes", "--origin", "external-content"}
	for i, w := range want {
		if argv[i] != w {
			t.Fatalf("argv[%d] = %q, want %q (full %v)", i, argv[i], w, argv)
		}
	}
	if !slices.Contains(argv, "--prompt-file") || !slices.Contains(argv, "--max-units") || !slices.Contains(argv, "--tools") {
		t.Fatalf("missing expected flags: %v", argv)
	}
}

func TestChatEmptyPrompt(t *testing.T) {
	if _, err := Chat("   ", ChatOptions{AppID: "x"}); err == nil {
		t.Fatal("expected error for empty prompt")
	}
}

func TestChatRequiresAppID(t *testing.T) {
	bin, _ := fakeCos(t, okChat, 0)
	var err error
	withCos(t, bin, map[string]string{"COS_APP_ID": ""}, func() {
		_, err = Chat("hi", ChatOptions{})
	})
	if _, ok := err.(*AiUnavailableError); !ok {
		t.Fatalf("expected AiUnavailableError, got %T %v", err, err)
	}
}

func TestChatErrorClassification(t *testing.T) {
	cases := []struct {
		body string
		want string
	}{
		{`{"error": "monthly budget exceeded"}`, "budget"},
		{`{"error": "prompt injection detected"}`, "safety"},
		{`{"error": "capability denied"}`, "denied"},
	}
	for _, c := range cases {
		bin, _ := fakeCos(t, c.body, 1)
		var err error
		withCos(t, bin, map[string]string{"COS_APP_ID": "notes"}, func() {
			_, err = Chat("hi", ChatOptions{})
		})
		switch c.want {
		case "budget":
			if _, ok := err.(*AiBudgetExceededError); !ok {
				t.Fatalf("%s: want AiBudgetExceededError, got %T", c.body, err)
			}
		case "safety":
			if _, ok := err.(*AiSafetyViolationError); !ok {
				t.Fatalf("%s: want AiSafetyViolationError, got %T", c.body, err)
			}
		default:
			if _, ok := err.(*AiDeniedError); !ok {
				t.Fatalf("%s: want AiDeniedError, got %T", c.body, err)
			}
		}
	}
}

func TestEmbedReturnsUnsupported(t *testing.T) {
	res, err := Embed("text", ChatOptions{})
	if res != nil {
		t.Fatal("Embed returned a response")
	}
	if _, ok := err.(*AiUnsupportedError); !ok {
		t.Fatalf("want AiUnsupportedError, got %T", err)
	}
}
