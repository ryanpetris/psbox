package config

// sandbox: one-hop resolution and effective launch selection.

import (
	"fmt"
	"sort"
	"strings"
)

func (c *Collection) resolveSandbox() {
	c.SandboxErrors = map[string]error{}
	for name, app := range c.Applications {
		if err := c.checkSandbox(name, app); err != nil {
			c.SandboxErrors[name] = err
		}
	}
}

func (c *Collection) checkSandbox(name string, app *Application) error {
	targetName := sandboxTarget(app)
	if targetName == "" {
		return nil
	}

	var offending []string
	opts := app.Spec.Options
	if opts.Home != nil {
		offending = append(offending, "options.home")
	}
	if opts.Audio != nil {
		offending = append(offending, "options.audio")
	}
	if opts.Video != nil {
		offending = append(offending, "options.video")
	}
	if opts.DBus != nil {
		offending = append(offending, "options.dbus")
	}
	if opts.Display != nil {
		offending = append(offending, "options.display")
	}
	if opts.Fontconfig != nil {
		offending = append(offending, "options.fontconfig")
	}
	if opts.Isolation != nil {
		offending = append(offending, "options.isolation")
	}
	if opts.HostURLs != nil {
		offending = append(offending, "options.host-urls")
	}
	if opts.Cache != nil {
		offending = append(offending, "options.cache")
	}
	if opts.Downloads != nil {
		offending = append(offending, "options.downloads")
	}
	if app.Type != "" {
		offending = append(offending, "metadata.type")
	}
	if len(app.Spec.BwrapArgs) > 0 {
		offending = append(offending, "bwrap-args")
	}
	if len(offending) > 0 {
		sort.Strings(offending)
		return fmt.Errorf("application %q sets sandbox: and also %s", name, strings.Join(offending, ", "))
	}

	target, ok := c.Applications[targetName]
	if !ok {
		return fmt.Errorf("application %q sandbox %q does not exist", name, targetName)
	}
	if sandboxTarget(target) != "" {
		return fmt.Errorf("application %q sandbox %q also sets a sandbox target", name, targetName)
	}
	return nil
}

func sandboxTarget(app *Application) string {
	if app == nil || app.Spec.Options.Sandbox == nil {
		return ""
	}
	return *app.Spec.Options.Sandbox
}

// SandboxTarget is the one-hop sandbox: application name, or empty.
func (a *Application) SandboxTarget() string {
	return sandboxTarget(a)
}

// LookupApplication returns a loaded application that is valid for listing.
func (c *Collection) LookupApplication(name string) (*Application, error) {
	app, ok := c.Applications[name]
	if !ok {
		return nil, fmt.Errorf("application %q not found", name)
	}
	return app, nil
}

// ListApplications returns sorted application names, omitting sandbox: errors.
func (c *Collection) ListApplications() []string {
	var names []string
	for name := range c.Applications {
		if _, bad := c.SandboxErrors[name]; bad {
			continue
		}
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

// RequireApplication returns the application or a collection-level sandbox: error.
func (c *Collection) RequireApplication(name string) (*Application, error) {
	app, err := c.LookupApplication(name)
	if err != nil {
		return nil, err
	}
	if err, bad := c.SandboxErrors[name]; bad {
		return nil, err
	}
	return app, nil
}

// Resolve returns the effective launch for name, applying an optional CLI isolation override.
func (c *Collection) Resolve(name string, override Isolation, hasOverride bool) (*Effective, error) {
	if err := CheckName("application", name); err != nil {
		return nil, err
	}
	app, err := c.RequireApplication(name)
	if err != nil {
		return nil, err
	}

	target := app
	sandboxName := app.Name
	if dest := sandboxTarget(app); dest != "" {
		t, ok := c.Applications[dest]
		if !ok {
			return nil, fmt.Errorf("application %q sandbox %q does not exist", name, dest)
		}
		target = t
		sandboxName = dest
	}

	iso, err := target.Isolation()
	if err != nil {
		return nil, err
	}
	if hasOverride {
		iso = override
	}

	env := map[string]string{}
	for k, v := range app.Spec.Env {
		env[k] = v
	}

	exec := append([]string(nil), app.Spec.Exec...)
	if len(exec) == 0 {
		exec = []string{app.Name}
	}

	return &Effective{
		Name:        app.Name,
		SandboxName: sandboxName,
		Target:      target,
		Exec:        exec,
		DefaultArgs: append([]string(nil), app.Spec.DefaultArgs...),
		Env:         env,
		Isolation:   iso,
	}, nil
}

// Isolation returns the configured isolation of an application.
func (a *Application) Isolation() (Isolation, error) {
	if a.Spec.Options.Isolation == nil || *a.Spec.Options.Isolation == "" {
		return Isolation{Mode: IsolationInstance, Name: DefaultInstance}, nil
	}
	return ParseIsolation(*a.Spec.Options.Isolation)
}

// DBus returns the effective dbus setting: host, private, or off.
func (a *Application) DBus() string {
	if a.Spec.Options.DBus == nil {
		return DBusHost
	}
	return *a.Spec.Options.DBus
}

// HostURLs reports whether the sandbox forwards http(s) to the host.
func (a *Application) HostURLs() bool {
	if a == nil {
		return true
	}
	return a.Spec.Options.BoolOption(a.Spec.Options.HostURLs)
}

// Downloads reports whether the host Downloads directory is bound in.
func (a *Application) Downloads() bool {
	if a == nil || a.Spec.Options.Downloads == nil {
		return false
	}
	return *a.Spec.Options.Downloads
}

// Cache returns the effective cache location: tmpfs or home.
func (a *Application) Cache() string {
	if a == nil || a.Spec.Options.Cache == nil || *a.Spec.Options.Cache == "" {
		return CacheTmpfs
	}
	return *a.Spec.Options.Cache
}

// BoolOption returns the named boolean option, defaulting to true when omitted.
func (o ApplicationOptions) BoolOption(ptr *bool) bool {
	if ptr == nil {
		return true
	}
	return *ptr
}

// HomeValue returns the configured home string, or empty if omitted.
func (o ApplicationOptions) HomeValue() string {
	if o.Home == nil {
		return ""
	}
	return *o.Home
}

// DesktopEntries returns desktop and autostart entries for application name.
func (c *Collection) DesktopEntries(application string) []*DesktopEntry {
	var out []*DesktopEntry
	for _, entry := range c.FreedesktopEntries {
		if entry.Application == application {
			out = append(out, entry)
		}
	}
	for _, entry := range c.FreedesktopAutostartEntries {
		if entry.Application == application {
			out = append(out, entry)
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Kind != out[j].Kind {
			return out[i].Kind < out[j].Kind
		}
		return out[i].Name < out[j].Name
	})
	return out
}
