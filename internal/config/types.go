package config

// Object kinds and parsed document types.

const (
	VersionV1 = "v1"

	KindApplication               = "Application"
	KindFreedesktopEntry          = "FreedesktopEntry"
	KindFreedesktopAutostartEntry = "FreedesktopAutostartEntry"
	KindOptions                   = "Options"
)

const (
	HomeNone  = ":none:"
	HomeTmpfs = ":tmpfs:"
	RootToken = ":root:"
	HomeToken = ":home:"

	IsolationOneshot  = "oneshot"
	IsolationInstance = "instance"
	DefaultInstance   = "default"

	DBusHost    = "host"
	DBusPrivate = "private"
	DBusOff     = "off"

	CacheTmpfs = "tmpfs"
	CacheHome  = "home"
)

// Isolation is a sandbox lifetime mode.
type Isolation struct {
	Mode string
	Name string
}

// String returns the isolation value in YAML/CLI form.
func (i Isolation) String() string {
	if i.Mode == IsolationOneshot {
		return IsolationOneshot
	}
	if i.Name != "" && i.Name != DefaultInstance {
		return IsolationInstance + ":" + i.Name
	}
	return IsolationInstance
}

// Instance reports whether this is instance isolation.
func (i Isolation) Instance() bool {
	return i.Mode != IsolationOneshot
}

// ApplicationOptions are the options: keys of an Application spec.
type ApplicationOptions struct {
	Home       *string
	Audio      *bool
	Video      *bool
	DBus       *string
	Display    *bool
	Fontconfig *bool
	Isolation  *string
	Sandbox    *string
	HostURLs   *bool
	Cache      *string
	Downloads  *bool
}

// ApplicationSpec is the spec: of an Application object.
type ApplicationSpec struct {
	Exec        []string
	DefaultArgs []string
	Env         map[string]string
	BwrapArgs   [][]string
	Options     ApplicationOptions
}

// Application is a named v1 Application object.
type Application struct {
	File string
	Name string
	Type string
	Spec ApplicationSpec
}

// Override rewrites a desktop-entry field with a regex replacement.
type Override struct {
	Section     string
	Field       string
	Pattern     string
	Replacement string
}

// Inject inserts a line after a matching desktop-entry section header.
type Inject struct {
	Section string
	Value   string
}

// DesktopEntry is a named FreedesktopEntry or FreedesktopAutostartEntry.
type DesktopEntry struct {
	File        string
	Name        string
	Kind        string
	Application string
	Value       string
	Overrides   []Override
	Inject      []Inject
}

// Collection is the loaded object set from one objects directory.
type Collection struct {
	Root                        string
	ObjectPath                  string
	Applications                map[string]*Application
	FreedesktopEntries          map[string]*DesktopEntry
	FreedesktopAutostartEntries map[string]*DesktopEntry
	// SandboxErrors maps a referring application name to a collection-level
	// sandbox: error. Those applications are omitted from list and
	// completions and rejected at launch, install, and render.
	SandboxErrors map[string]error
}

// Effective is one launch after resolving sandbox: and CLI isolation.
type Effective struct {
	Name        string
	SandboxName string
	Target      *Application
	Exec        []string
	DefaultArgs []string
	Env         map[string]string
	Isolation   Isolation
}
