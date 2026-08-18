package config

// Environment overlay validation.

import (
	"fmt"
	"strings"
	"unicode"
)

// ValidateEnv checks that keys and values are legal process environment strings.
func ValidateEnv(env map[string]string) error {
	for k, v := range env {
		if err := CheckEnvKey(k); err != nil {
			return err
		}
		if err := CheckEnvValue(k, v); err != nil {
			return err
		}
	}
	return nil
}

// CheckEnvKey reports whether key is a legal environment variable name.
func CheckEnvKey(key string) error {
	if key == "" {
		return fmt.Errorf("environment variable name is empty")
	}
	if strings.ContainsRune(key, '=') || strings.ContainsRune(key, 0) {
		return fmt.Errorf("invalid environment variable name %q", key)
	}
	runes := []rune(key)
	if !envKeyStart(runes[0]) {
		return fmt.Errorf("invalid environment variable name %q", key)
	}
	for _, r := range runes[1:] {
		if !envKeyRune(r) {
			return fmt.Errorf("invalid environment variable name %q", key)
		}
	}
	return nil
}

// CheckEnvValue reports whether value is a legal environment variable value.
func CheckEnvValue(key, value string) error {
	if strings.ContainsRune(value, 0) {
		return fmt.Errorf("environment variable %q contains a NUL", key)
	}
	return nil
}

func envKeyStart(r rune) bool {
	return r == '_' || unicode.IsLetter(r)
}

func envKeyRune(r rune) bool {
	return r == '_' || unicode.IsLetter(r) || unicode.IsDigit(r)
}
