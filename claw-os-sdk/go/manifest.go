package clawossdk

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

// ValidateOwnerPath validates request data, not a grant or filesystem mount.
func ValidateOwnerPath(verb, path string) error {
	if err := ValidateOwnerPathNeed(map[string]any{"verb": verb, "scope": map[string]any{"kind": "owner-path", "path": path}}); err != nil {
		return err
	}
	if !utf8.ValidString(path) || len(path) > 4096 {
		return fmt.Errorf("owner-path exceeds its UTF-8 path bound")
	}
	for _, part := range strings.Split(path, "/") {
		if len(part) > 255 {
			return fmt.Errorf("owner-path exceeds its UTF-8 segment bound")
		}
	}
	return nil
}

// ResolveOwnerPath is a lexical preview; Root independently supplies verified home.
func ResolveOwnerPath(verb, path, ownerHome string) (string, error) {
	if err := ValidateOwnerPath(verb, path); err != nil {
		return "", err
	}
	if !strings.HasPrefix(ownerHome, "/") || strings.Contains(ownerHome, "*") {
		return "", fmt.Errorf("owner home must be absolute and contain no pattern")
	}
	if err := ValidateOwnerPath("fs.read", ownerHome[1:]); err != nil {
		return "", err
	}
	resolved := ownerHome + "/" + path
	if len(resolved) > 4096 {
		return "", fmt.Errorf("resolved owner path exceeds 4096 UTF-8 bytes")
	}
	return resolved, nil
}

func validateOwnerPathBindings(manifest map[string]any) error {
	var entries []any
	var guiEntries []map[string]any
	if operations, ok := manifest["operations"].(map[string]any); ok {
		for _, entry := range operations {
			entries = append(entries, entry)
		}
	}
	if service, ok := manifest["mcp"].(map[string]any); ok {
		if tools, ok := service["tools"].([]any); ok {
			entries = append(entries, tools...)
		}
	}
	if desktop, ok := manifest["desktop"].(map[string]any); ok {
		entries = append(entries, desktop)
		guiEntries = append(guiEntries, desktop)
		if extras, ok := desktop["entries"].(map[string]any); ok {
			for _, entry := range extras {
				entries = append(entries, entry)
				if gui, ok := entry.(map[string]any); ok {
					guiEntries = append(guiEntries, gui)
				}
			}
		}
	}
	for _, raw := range entries {
		entry, ok := raw.(map[string]any)
		if !ok {
			continue
		}
		needs, _ := entry["needs"].([]any)
		if err := validateOwnerPathNeeds(needs); err != nil {
			return err
		}
	}
	for _, gui := range guiEntries {
		if raw, present := gui["optional_needs"]; present {
			needs, ok := raw.([]any)
			if !ok {
				return fmt.Errorf("GUI optional_needs must be an array")
			}
			if err := validateOwnerPathNeeds(needs); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateOwnerPathNeeds(needs []any) error {
	for _, rawNeed := range needs {
		need, ok := rawNeed.(map[string]any)
		if !ok {
			continue
		}
		scope, _ := need["scope"].(map[string]any)
		if scope["kind"] == "owner-path" {
			if err := ValidateOwnerPathNeed(need); err != nil {
				return err
			}
			if err := ValidateOwnerPath(need["verb"].(string), scope["path"].(string)); err != nil {
				return err
			}
		}
	}
	return nil
}
