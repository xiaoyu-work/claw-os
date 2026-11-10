package clawossdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestOwnerPathSharedVectors(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "wire", "v1", "owner_path.vectors.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors struct {
		Cases []struct {
			Name, Verb, Path, Home string
			Expected               *string
		}
	}
	if err := json.Unmarshal(data, &vectors); err != nil {
		t.Fatal(err)
	}
	for _, entry := range vectors.Cases {
		t.Run(entry.Name, func(t *testing.T) {
			got, err := ResolveOwnerPath(entry.Verb, entry.Path, entry.Home)
			if entry.Expected == nil {
				if err == nil {
					t.Fatalf("unbounded/invalid preview accepted: %s", got)
				}
			} else if err != nil || got != *entry.Expected {
				t.Fatalf("got %q, %v", got, err)
			}
		})
	}

	if ValidateOwnerPath("fs.read", strings.Repeat("é", 128)) == nil {
		t.Fatal("UTF-8 segment bound not enforced")
	}
	for _, field := range []string{"owner_uid", "home", "owner_home"} {
		if ValidateOwnerPathNeed(map[string]any{"verb": "fs.read", "scope": map[string]any{"kind": "owner-path", "path": "Documents/**", field: "/home/bob"}}) == nil {
			t.Fatalf("caller-controlled owner selector accepted: %s", field)
		}
	}

}

func TestOptionalGUIOwnerPathValidation(t *testing.T) {
	for _, invalid := range []any{nil, "not-an-array", map[string]any{}, []any{
		map[string]any{"verb": "fs.read", "scope": map[string]any{"kind": "owner-path", "path": "../other/**"}},
	}} {
		for _, manifest := range []map[string]any{
			{"desktop": map[string]any{"optional_needs": invalid}},
			{"desktop": map[string]any{"entries": map[string]any{"applet": map[string]any{"optional_needs": invalid}}}},
		} {
			if validateOwnerPathBindings(manifest) == nil {
				t.Fatal("invalid optional GUI declaration accepted")
			}
		}
	}
	if err := validateOwnerPathBindings(map[string]any{"desktop": map[string]any{"optional_needs": []any{}}}); err != nil {
		t.Fatal(err)
	}
}
