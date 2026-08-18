package config

// Name grammar for applications, instances, and object metadata.

import (
	"fmt"
	"regexp"
)

var namePattern = regexp.MustCompile(`^[A-Za-z0-9._][A-Za-z0-9._-]*$`)

// ValidName reports whether name matches the object and instance grammar.
func ValidName(name string) bool {
	return namePattern.MatchString(name)
}

// CheckName returns an error if name is empty or illegal.
func CheckName(kind, name string) error {
	if name == "" {
		return fmt.Errorf("%s name is empty", kind)
	}
	if !ValidName(name) {
		return fmt.Errorf("invalid %s name %q", kind, name)
	}
	return nil
}
