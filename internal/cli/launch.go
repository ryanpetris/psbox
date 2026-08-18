package cli

// Launch flags and command.

import (
	"fmt"

	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/complete"
	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/launch"
)

func newLaunchCommand(svc *launch.Service, paths config.Paths) *cobra.Command {
	var req launch.Request
	cmd := &cobra.Command{
		Use:               "launch [flags] <application> [-- args]",
		Short:             "Run an application in a private-home sandbox",
		Args:              cobra.ArbitraryArgs,
		ValidArgsFunction: complete.Applications,
		RunE: func(cmd *cobra.Command, args []string) error {
			if len(args) == 0 {
				_ = cmd.Help()
				return errHelp
			}
			req.Application = args[0]
			req.Args = args[1:]
			req.ProgramName = "psbox"
			return svc.Run(cmd.Context(), req, cmd.OutOrStdout())
		},
	}
	cmd.Flags().StringVar(&req.Objects, "objects", paths.ObjectPath, "path to configuration objects")
	cmd.Flags().BoolVar(&req.Print, "print", false, "print the oneshot psbox sandbox command instead of running it")
	cmd.Flags().BoolVar(&req.PrintBwrap, "print-bwrap", false, "print the bwrap command instead of running it")
	cmd.Flags().StringVar(&req.Isolation, "isolation", "", "override isolation (`oneshot`, `instance`, or `instance:name`)")
	cmd.Flags().StringVar(&req.Instance, "instance", "", "join or create a named instance")
	cmd.Flags().BoolVar(&req.Command, "command", false, "treat the argument list as the full command")
	cmd.Flags().SetInterspersed(true)
	return cmd
}

var errHelp = fmt.Errorf("help requested")
