package clawossdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type objectVectors struct {
	Valid []struct {
		Reference ObjectRef `json:"reference"`
		URI       string    `json:"uri"`
	} `json:"valid"`
	InvalidURIs []string    `json:"invalid_uris"`
	InvalidRefs []ObjectRef `json:"invalid_refs"`
	Limits      []struct {
		Field string `json:"field"`
		Unit  string `json:"unit"`
		Count int    `json:"count"`
		Bytes int    `json:"bytes"`
	} `json:"limits"`
	WireCases []struct {
		Value json.RawMessage `json:"value"`
		Code  *string         `json:"code"`
		Path  *string         `json:"path"`
	} `json:"wire_cases"`
}

func loadObjectVectors(t *testing.T) objectVectors {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "wire", "v1", "object_ref.vectors.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors objectVectors
	if err := json.Unmarshal(data, &vectors); err != nil {
		t.Fatal(err)
	}
	return vectors
}

func objectReference() ObjectRef {
	return ObjectRef{AppId: "notes", ObjectType: "note", ObjectId: "x"}
}

func setObjectField(t *testing.T, value *ObjectRef, field, text string) {
	t.Helper()
	switch field {
	case "app_id":
		value.AppId = text
	case "object_type":
		value.ObjectType = text
	case "object_id":
		value.ObjectId = text
	case "revision":
		value.Revision = &text
	default:
		t.Fatalf("unknown fixture field %s", field)
	}
}

func TestObjectReferenceSharedCanonicalVectors(t *testing.T) {
	for _, entry := range loadObjectVectors(t).Valid {
		uri, err := FormatReference(entry.Reference)
		if err != nil || uri != entry.URI {
			t.Fatalf("FormatReference = %q, %v; want %q", uri, err, entry.URI)
		}
		parsed, err := ParseReference(uri)
		if err != nil || !reflect.DeepEqual(parsed, entry.Reference) {
			t.Fatalf("ParseReference(%q) = %+v, %v; want %+v", uri, parsed, err, entry.Reference)
		}
	}
}

func TestObjectReferenceSharedInvalidVectors(t *testing.T) {
	vectors := loadObjectVectors(t)
	for _, uri := range vectors.InvalidURIs {
		if _, err := ParseReference(uri); err == nil {
			t.Fatalf("accepted invalid URI %q", uri)
		}
	}
	for _, value := range vectors.InvalidRefs {
		if _, err := FormatReference(value); err == nil {
			t.Fatalf("accepted invalid reference %+v", value)
		}
	}
}

func TestObjectReferenceSharedByteLimits(t *testing.T) {
	for _, limit := range loadObjectVectors(t).Limits {
		text := strings.Repeat(limit.Unit, limit.Count)
		if len(text) != limit.Bytes {
			t.Fatalf("%s has %d bytes, want %d", limit.Field, len(text), limit.Bytes)
		}
		value := objectReference()
		setObjectField(t, &value, limit.Field, text)
		uri, err := FormatReference(value)
		if err != nil {
			t.Fatal(err)
		}
		parsed, err := ParseReference(uri)
		if err != nil || !reflect.DeepEqual(parsed, value) {
			t.Fatalf("boundary round trip = %+v, %v", parsed, err)
		}
		setObjectField(t, &value, limit.Field, text+"x")
		if _, err := FormatReference(value); err == nil {
			t.Fatalf("accepted oversized %s", limit.Field)
		}
	}
}

func TestObjectReferenceMaximumComponentsAndURICeiling(t *testing.T) {
	revision := strings.Repeat("\u00e9", 64)
	value := ObjectRef{
		AppId:      strings.Repeat("a", 128),
		ObjectType: strings.Repeat("a", 64),
		ObjectId:   strings.Repeat("\u00e9", 512),
		Revision:   &revision,
	}
	uri, err := FormatReference(value)
	if err != nil || len(uri) != 3669 {
		t.Fatalf("maximum URI: length=%d, error=%v", len(uri), err)
	}
	if _, err := ParseReference(uri); err != nil {
		t.Fatal(err)
	}
	if _, err := ParseReference("app://notes/note?id=" + strings.Repeat("x", 4096)); err == nil {
		t.Fatal("accepted an oversized URI")
	}
}

func TestObjectReferenceInvalidUTF8NeverBecomesAnotherIdentity(t *testing.T) {
	for _, text := range []string{
		string([]byte{0xff}),
		string([]byte{0xed, 0xa0, 0x80}),
		string([]byte{0xf4, 0x90, 0x80, 0x80}),
	} {
		for _, field := range []string{"object_id", "revision"} {
			value := objectReference()
			setObjectField(t, &value, field, text)
			if _, err := FormatReference(value); err == nil {
				t.Fatalf("accepted invalid UTF-8 %s", field)
			}
		}
		if _, err := ParseReference("app://notes/note?id=" + text); err == nil {
			t.Fatal("accepted invalid UTF-8 URI")
		}
	}
}

func TestObjectReferenceGeneratedDecoderSharedContract(t *testing.T) {
	for _, entry := range loadObjectVectors(t).WireCases {
		value := decodeWireValue(t, string(entry.Value))
		err := ValidateObjectRef(value)
		if entry.Code != nil {
			wireErr, ok := err.(*WireDecodeError)
			if !ok || wireErr.Code != *entry.Code || entry.Path == nil || wireErr.Path != *entry.Path {
				t.Fatalf("wire error = %#v, expected code=%v path=%v", err, entry.Code, entry.Path)
			}
			continue
		}
		if err != nil {
			t.Fatal(err)
		}
		var typed ObjectRef
		if err := json.Unmarshal(entry.Value, &typed); err != nil {
			t.Fatal(err)
		}
		encoded, err := json.Marshal(typed)
		if err != nil || !reflect.DeepEqual(decodeWireValue(t, string(encoded)), value) {
			t.Fatalf("wire round trip = %s, %v", encoded, err)
		}
	}
}

func TestObjectReferenceLegacyCompatibilityAndOptionalRevision(t *testing.T) {
	body := `{"id":"notes","version":"1.0.0","name":{"en":"Notes"}}`
	var manifest Manifest
	if err := json.Unmarshal([]byte(body), &manifest); err != nil {
		t.Fatal(err)
	}
	encoded, err := json.Marshal(manifest)
	if err != nil || !reflect.DeepEqual(decodeWireValue(t, string(encoded)), decodeWireValue(t, body)) {
		t.Fatalf("legacy manifest changed: %s, %v", encoded, err)
	}
	value := objectReference()
	if _, err := FormatReference(value); err != nil {
		t.Fatal(err)
	}
	empty := ""
	value.Revision = &empty
	if _, err := FormatReference(value); err == nil {
		t.Fatal("a present empty revision must not become an absent revision")
	}
}
