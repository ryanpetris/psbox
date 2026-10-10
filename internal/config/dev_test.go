package config

// Device option parsing, defaults, and sandbox ownership tests.

import (
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

func TestDevOption(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		yaml string
		want string
	}{
		{"omitted", "{}", DevPrivate},
		{"private", "dev: private", DevPrivate},
		{"host", "dev: host", DevHost},
		{"true", "dev: true", DevHost},
		{"capitalized true", "dev: True", DevHost},
		{"uppercase true", "dev: TRUE", DevHost},
		{"quoted true", `dev: "true"`, DevHost},
		{"private shared memory", "dev: private-shm", DevPrivateSHM},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var raw map[string]yaml.Node
			if err := yaml.Unmarshal([]byte(tc.yaml), &raw); err != nil {
				t.Fatal(err)
			}
			opts, err := decodeApplicationOptions(raw)
			if err != nil {
				t.Fatal(err)
			}
			app := &Application{Spec: ApplicationSpec{Options: opts}}
			if got := app.Dev(); got != tc.want {
				t.Fatalf("Dev() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestDevOptionValidation(t *testing.T) {
	t.Parallel()

	for _, value := range []string{"false", "False", "FALSE", "off", "y", "Y", "yes", "Yes", "YES", "on", "On", "ON", `"on"`, `"yes"`, `"y"`, `"True"`, `"TRUE"`, `""`, "null", "[]", "[host]", "{}", "123"} {
		t.Run(value, func(t *testing.T) {
			var raw map[string]yaml.Node
			if err := yaml.Unmarshal([]byte("dev: "+value), &raw); err != nil {
				t.Fatal(err)
			}
			if _, err := decodeApplicationOptions(raw); err == nil || !strings.Contains(err.Error(), "options.dev") {
				t.Fatalf("expected dev validation error, got %v", err)
			}
		})
	}
	for _, mode := range []string{DevHost, DevPrivateSHM} {
		for _, option := range []string{"audio", "video", "display"} {
			t.Run(mode+"/"+option, func(t *testing.T) {
				var raw map[string]yaml.Node
				if err := yaml.Unmarshal([]byte("dev: "+mode+"\n"+option+": false"), &raw); err != nil {
					t.Fatal(err)
				}
				if _, err := decodeApplicationOptions(raw); err == nil || !strings.Contains(err.Error(), "options."+option) {
					t.Fatalf("expected conflicting option error, got %v", err)
				}
			})
		}
	}
}

func TestDevReferrerRejected(t *testing.T) {
	t.Parallel()

	target, mode := "owner", DevPrivate
	app := &Application{Spec: ApplicationSpec{Options: ApplicationOptions{Sandbox: &target, Dev: &mode}}}
	col := &Collection{Applications: map[string]*Application{"owner": {Name: "owner"}, "referrer": app}}
	if err := col.checkSandbox("referrer", app); err == nil || !strings.Contains(err.Error(), "options.dev") {
		t.Fatalf("expected referrer ownership error, got %v", err)
	}
}
