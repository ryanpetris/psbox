package cli

// Positional-argument checks with user-facing error text.

import (
	"fmt"

	"github.com/spf13/cobra"
)

func noArgs(name string) cobra.PositionalArgs {
	return func(cmd *cobra.Command, args []string) error {
		if len(args) > 0 {
			return fmt.Errorf("%s takes no arguments", name)
		}
		return nil
	}
}

func listArgs(cmd *cobra.Command, args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("missing list kind; use applications, desktop, or autostart")
	}
	if len(args) > 1 {
		return fmt.Errorf("list takes one kind; use applications, desktop, or autostart")
	}
	switch args[0] {
	case "applications", "desktop", "autostart":
		return nil
	default:
		return fmt.Errorf("unknown list kind %q; use applications, desktop, or autostart", args[0])
	}
}

func renderArgs(cmd *cobra.Command, args []string) error {
	if len(args) != 1 {
		return fmt.Errorf("usage: psbox objects render <application>")
	}
	return nil
}
