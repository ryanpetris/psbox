package config

// Isolation value parsing for YAML and CLI flags.

import (
	"fmt"
	"strings"
)

// ParseIsolation parses oneshot, instance, or instance:<name>.
func ParseIsolation(val string) (Isolation, error) {
	if val == "" || val == IsolationInstance {
		return Isolation{Mode: IsolationInstance, Name: DefaultInstance}, nil
	}
	if val == IsolationOneshot {
		return Isolation{Mode: IsolationOneshot}, nil
	}
	if rest, ok := strings.CutPrefix(val, IsolationInstance+":"); ok {
		if err := CheckName("instance", rest); err != nil {
			return Isolation{}, err
		}
		return Isolation{Mode: IsolationInstance, Name: rest}, nil
	}
	return Isolation{}, fmt.Errorf("invalid isolation %q", val)
}

// IsolationFromFlags combines --isolation and --instance. They are mutually exclusive.
func IsolationFromFlags(isolationFlag, instanceFlag string) (Isolation, bool, error) {
	if isolationFlag != "" && instanceFlag != "" {
		return Isolation{}, false, fmt.Errorf("--isolation and --instance are mutually exclusive")
	}
	if instanceFlag != "" {
		if err := CheckName("instance", instanceFlag); err != nil {
			return Isolation{}, false, err
		}
		return Isolation{Mode: IsolationInstance, Name: instanceFlag}, true, nil
	}
	if isolationFlag != "" {
		iso, err := ParseIsolation(isolationFlag)
		if err != nil {
			return Isolation{}, false, err
		}
		return iso, true, nil
	}
	return Isolation{}, false, nil
}
