package config

// Application types and the option defaults they fill.

import "fmt"

const TypeBrowser = "browser"

func parseApplicationType(val string) (string, error) {
	switch val {
	case "", TypeBrowser:
		return val, nil
	default:
		return "", fmt.Errorf("unknown application type %q", val)
	}
}

func applyTypeDefaults(app *Application) {
	if app == nil {
		return
	}
	switch app.Type {
	case TypeBrowser:
		if app.Spec.Options.DBus == nil {
			v := DBusPrivate
			app.Spec.Options.DBus = &v
		}
		if app.Spec.Options.HostURLs == nil {
			v := false
			app.Spec.Options.HostURLs = &v
		}
		if app.Spec.Options.Downloads == nil {
			v := true
			app.Spec.Options.Downloads = &v
		}
	}
}
