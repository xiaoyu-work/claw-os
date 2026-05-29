package clawossdk

import "testing"

func TestToolBuildsArgvAndParses(t *testing.T) {
	body := `{"tool": "fs.read_text", "app_id": "notes", "status": "ok", "result": {"content": "hi"}}`
	bin, argvOut := fakeCos(t, body, 0)
	var res *ToolResult
	var err error
	withCos(t, bin, map[string]string{"COS_APP_ID": "notes"}, func() {
		res, err = CallTool("fs.read_text", map[string]any{"path": "/x"}, "")
	})
	if err != nil {
		t.Fatalf("Tool error: %v", err)
	}
	if res.Name != "fs.read_text" || res.AppID != "notes" || res.Status != "ok" {
		t.Fatalf("result = %+v", res)
	}
	if m, ok := res.Value.(map[string]any); !ok || m["content"] != "hi" {
		t.Fatalf("value = %v", res.Value)
	}
	argv := readArgv(t, argvOut)
	want := []string{"ai", "tool", "fs.read_text", "--app", "notes", "--args"}
	for i, w := range want {
		if argv[i] != w {
			t.Fatalf("argv[%d] = %q, want %q (full %v)", i, argv[i], w, argv)
		}
	}
	if argv[6] != `{"path":"/x"}` {
		t.Fatalf("args json = %q", argv[6])
	}
}

func TestToolEmptyName(t *testing.T) {
	if _, err := CallTool("  ", nil, "notes"); err == nil {
		t.Fatal("expected error for empty name")
	}
}

func TestToolRequiresAppID(t *testing.T) {
	bin, _ := fakeCos(t, `{}`, 0)
	var err error
	withCos(t, bin, map[string]string{"COS_APP_ID": ""}, func() {
		_, err = CallTool("fs.read_text", nil, "")
	})
	if _, ok := err.(*ToolUnavailableError); !ok {
		t.Fatalf("expected ToolUnavailableError, got %T %v", err, err)
	}
}

func TestToolDenied(t *testing.T) {
	bin, _ := fakeCos(t, `{"error": "capability denied: fs.read"}`, 1)
	var err error
	withCos(t, bin, map[string]string{"COS_APP_ID": "notes"}, func() {
		_, err = CallTool("fs.read_text", nil, "")
	})
	if _, ok := err.(*ToolDeniedError); !ok {
		t.Fatalf("expected ToolDeniedError, got %T %v", err, err)
	}
}

func TestCatalogParses(t *testing.T) {
	body := `{"tools": [
		{"name": "fs.read_text", "summary": "read", "verb": "fs.read", "stability": "stable",
		 "args_schema": {"type": "object"}, "returns_schema": "{\"type\":\"string\"}"},
		{"name": "kv.get"}
	]}`
	bin, argvOut := fakeCos(t, body, 0)
	var entries []CatalogEntry
	var err error
	withCos(t, bin, nil, func() {
		entries, err = Catalog()
	})
	if err != nil {
		t.Fatalf("Catalog error: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("len = %d", len(entries))
	}
	if entries[0].Name != "fs.read_text" || entries[0].Stability != "stable" {
		t.Fatalf("entry0 = %+v", entries[0])
	}
	if entries[0].ReturnsSchema["type"] != "string" {
		t.Fatalf("returns_schema not parsed from string: %v", entries[0].ReturnsSchema)
	}
	if entries[1].Stability != "experimental" {
		t.Fatalf("missing stability should default to experimental, got %q", entries[1].Stability)
	}
	// Catalog must NOT pass --app or a `list` subcommand.
	argv := readArgv(t, argvOut)
	if len(argv) != 2 || argv[0] != "ai" || argv[1] != "tools" {
		t.Fatalf("catalog argv = %v, want [ai tools]", argv)
	}
}

func TestForChatTrimsAndDrops(t *testing.T) {
	got := ForChat("fs.read_text", " kv.get ", "", "  ")
	if len(got) != 2 || got[0] != "fs.read_text" || got[1] != "kv.get" {
		t.Fatalf("ForChat = %v", got)
	}
}
