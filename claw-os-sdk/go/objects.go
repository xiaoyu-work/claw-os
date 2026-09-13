package clawossdk

import (
	"net/url"
	"strings"
	"unicode"
	"unicode/utf8"
)

const maxObjectRefURIBytes = 4096

// ObjectRefError reports invalid object-reference components or canonical spelling.
type ObjectRefError struct{ Message string }

func (e *ObjectRefError) Error() string { return "invalid App object reference: " + e.Message }

func validateObjectReference(value ObjectRef) error {
	if !objectComponent(value.AppId, 128) {
		return &ObjectRefError{"app_id must be a lowercase ASCII component of at most 128 bytes"}
	}
	if !objectComponent(value.ObjectType, 64) {
		return &ObjectRefError{"object_type must be a lowercase ASCII component of at most 64 bytes"}
	}
	if !objectOpaque(value.ObjectId, 1024) {
		return &ObjectRefError{"object_id must contain 1..=1024 UTF-8 bytes without control characters"}
	}
	if value.Revision != nil && !objectOpaque(*value.Revision, 128) {
		return &ObjectRefError{"revision must contain 1..=128 UTF-8 bytes without control characters"}
	}
	return nil
}

// FormatReference formats an opaque identifier without resolving it or granting access.
func FormatReference(value ObjectRef) (string, error) {
	if err := validateObjectReference(value); err != nil {
		return "", err
	}
	uri := "app://" + value.AppId + "/" + value.ObjectType + "?id=" + encodeObjectQuery(value.ObjectId)
	if value.Revision != nil {
		uri += "&revision=" + encodeObjectQuery(*value.Revision)
	}
	if len(uri) > maxObjectRefURIBytes {
		return "", &ObjectRefError{"URI exceeds 4096 UTF-8 bytes"}
	}
	return uri, nil
}

// ParseReference accepts only canonical app:// references and performs no I/O.
func ParseReference(value string) (ObjectRef, error) {
	if len(value) > maxObjectRefURIBytes {
		return ObjectRef{}, &ObjectRefError{"URI exceeds 4096 UTF-8 bytes"}
	}
	if !utf8.ValidString(value) {
		return ObjectRef{}, &ObjectRefError{"URI must be valid UTF-8"}
	}
	rest, found := strings.CutPrefix(value, "app://")
	if !found {
		return ObjectRef{}, &ObjectRefError{"URI must use the exact app:// scheme"}
	}
	address, query, found := strings.Cut(rest, "?")
	if !found {
		return ObjectRef{}, &ObjectRefError{"URI requires an id query parameter"}
	}
	appID, objectType, found := strings.Cut(address, "/")
	if !found {
		return ObjectRef{}, &ObjectRefError{"URI requires one App and one object type"}
	}
	reference := ObjectRef{AppId: appID, ObjectType: objectType}
	idPresent := false
	for _, parameter := range strings.Split(query, "&") {
		key, encoded, found := strings.Cut(parameter, "=")
		if !found {
			return ObjectRef{}, &ObjectRefError{"invalid query parameter"}
		}
		switch {
		case key == "id" && !idPresent:
			decoded, err := decodeObjectQuery(encoded)
			if err != nil {
				return ObjectRef{}, err
			}
			reference.ObjectId = decoded
			idPresent = true
		case key == "revision" && reference.Revision == nil:
			decoded, err := decodeObjectQuery(encoded)
			if err != nil {
				return ObjectRef{}, err
			}
			reference.Revision = &decoded
		default:
			return ObjectRef{}, &ObjectRefError{"unknown or repeated query parameter"}
		}
	}
	if !idPresent {
		return ObjectRef{}, &ObjectRefError{"URI requires an id query parameter"}
	}
	canonical, err := FormatReference(reference)
	if err != nil {
		return ObjectRef{}, err
	}
	if canonical != value {
		return ObjectRef{}, &ObjectRefError{"URI is not canonically spelled"}
	}
	return reference, nil
}

func objectComponent(value string, limit int) bool {
	if len(value) == 0 || len(value) > limit || value[0] < 'a' || value[0] > 'z' {
		return false
	}
	for index := 0; index < len(value); index++ {
		c := value[index]
		if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '_' || c == '-') {
			return false
		}
	}
	return true
}

func objectOpaque(value string, limit int) bool {
	if len(value) == 0 || len(value) > limit || !utf8.ValidString(value) {
		return false
	}
	for _, character := range value {
		if unicode.IsControl(character) {
			return false
		}
	}
	return true
}

func encodeObjectQuery(value string) string {
	return strings.ReplaceAll(url.QueryEscape(value), "+", "%20")
}

func decodeObjectQuery(value string) (string, error) {
	for index := 0; index < len(value); index++ {
		c := value[index]
		if c >= 'A' && c <= 'Z' || c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || strings.ContainsRune("-._~", rune(c)) {
			continue
		}
		if c != '%' || index+2 >= len(value) || !objectHex(value[index+1]) || !objectHex(value[index+2]) {
			return "", &ObjectRefError{"query values must use RFC3986 percent encoding"}
		}
		index += 2
	}
	decoded, err := url.QueryUnescape(value)
	if err != nil || !utf8.ValidString(decoded) {
		return "", &ObjectRefError{"query value is not valid UTF-8"}
	}
	return decoded, nil
}

func objectHex(value byte) bool {
	return value >= '0' && value <= '9' || value >= 'A' && value <= 'F' || value >= 'a' && value <= 'f'
}
