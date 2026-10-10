package config

// Device filesystem modes and their configuration constraints.

import "fmt"

// ParseDev validates a device filesystem mode and normalizes true to host.
func ParseDev(value string) (string, error) {
	switch value {
	case "true", DevHost:
		return DevHost, nil
	case DevPrivate, DevPrivateSHM:
		return value, nil
	default:
		return "", fmt.Errorf("invalid dev mode %q; use true, host, private, or private-shm", value)
	}
}

// Dev returns the application's device filesystem mode, defaulting to private.
func (a *Application) Dev() string {
	if a == nil || a.Spec.Options.Dev == nil {
		return DevPrivate
	}
	return *a.Spec.Options.Dev
}

func validateDevOptions(opts ApplicationOptions) error {
	if opts.Dev == nil || *opts.Dev == DevPrivate {
		return nil
	}
	for _, option := range []struct {
		name  string
		value *bool
	}{
		{"audio", opts.Audio},
		{"video", opts.Video},
		{"display", opts.Display},
	} {
		if option.value != nil && !*option.value {
			return fmt.Errorf("options.dev %q conflicts with options.%s: false", *opts.Dev, option.name)
		}
	}
	return nil
}
