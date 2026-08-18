package cli

// Positional-argument error text tests.

import (
	"strings"
	"testing"
)

func TestFriendlyArgErrors(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		args []string
		want string
	}{
		{
			name: "list missing kind",
			args: []string{"objects", "list"},
			want: "missing list kind; use applications, desktop, or autostart",
		},
		{
			name: "list extra words",
			args: []string{"objects", "list", "applications", "desktop"},
			want: "list takes one kind; use applications, desktop, or autostart",
		},
		{
			name: "list unknown kind",
			args: []string{"objects", "list", "widgets"},
			want: `unknown list kind "widgets"; use applications, desktop, or autostart`,
		},
		{
			name: "render missing args",
			args: []string{"objects", "render"},
			want: "usage: psbox objects render <application>",
		},
		{
			name: "render extra args",
			args: []string{"objects", "render", "app", "icon"},
			want: "usage: psbox objects render <application>",
		},
		{
			name: "install extra args",
			args: []string{"objects", "install", "extra"},
			want: "install takes no arguments",
		},
		{
			name: "sandbox missing home",
			args: []string{"sandbox", "--", "/bin/true"},
			want: "one of --home, --home-tmpfs, or --no-home is required",
		},
		{
			name: "sandbox missing command",
			args: []string{"sandbox", "--home-tmpfs"},
			want: "missing command; use: psbox sandbox [flags] -- <command>",
		},
	}

	objects := t.TempDir()
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := execPrint(t, objects, tc.args)
			if err == nil {
				t.Fatal("expected error")
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error %q does not contain %q", err, tc.want)
			}
			if strings.Contains(err.Error(), "accepts") {
				t.Fatalf("stock cobra count error leaked: %v", err)
			}
		})
	}
}
