package cli

// Root command construction.

import (
	"os"

	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/launch"
	"petris.dev/psbox/internal/objects"
	"petris.dev/psbox/internal/sandbox"
	"petris.dev/psbox/internal/units"
	"petris.dev/psbox/internal/version"
)

// NewRoot builds the psbox command tree. Launch is a required subcommand.
func NewRoot(launchSvc *launch.Service, objectsSvc *objects.Service, sandboxSvc *sandbox.Service, unitsSvc *units.Service, paths config.Paths) *cobra.Command {
	root := &cobra.Command{
		Use:     "psbox",
		Short:   "Run applications in private-home sandboxes",
		Version: version.Current,
	}
	root.SilenceUsage = true
	root.CompletionOptions.DisableDefaultCmd = false
	root.SetOut(os.Stdout)
	root.SetErr(os.Stderr)
	root.AddCommand(
		newLaunchCommand(launchSvc, paths),
		newInstanceCommand(unitsSvc),
		newObjectsCommand(objectsSvc, paths),
		newSandboxCommand(sandboxSvc),
	)
	root.InitDefaultCompletionCmd()
	return root
}
