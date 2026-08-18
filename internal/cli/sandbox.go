package cli

// Sandbox subcommand.

import (
	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/complete"
	"petris.dev/psbox/internal/sandbox"
)

func newSandboxCommand(svc *sandbox.Service) *cobra.Command {
	var req sandbox.Request
	cmd := &cobra.Command{
		Use:               "sandbox [flags] -- <command>",
		Short:             "Run a raw command in a composed sandbox",
		Args:              cobra.ArbitraryArgs,
		ValidArgsFunction: complete.CommandArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			req.Command = args
			return svc.Run(req, cmd.OutOrStdout())
		},
	}
	cmd.Flags().StringVar(&req.Home, "home", "", "private home directory to use")
	cmd.Flags().BoolVar(&req.HomeTmpfs, "home-tmpfs", false, "use an empty tmpfs for the home directory")
	cmd.Flags().BoolVar(&req.NoHome, "no-home", false, "disable home directory support")
	cmd.Flags().BoolVar(&req.NoAudio, "no-audio", false, "disable audio support")
	cmd.Flags().BoolVar(&req.NoVideo, "no-video", false, "disable video support")
	cmd.Flags().BoolVar(&req.NoDBus, "no-dbus", false, "disable system and session dbus")
	cmd.Flags().BoolVar(&req.NoSessionDBus, "no-session-dbus", false, "do not bind the host session bus")
	cmd.Flags().BoolVar(&req.NoFontconfig, "no-fontconfig", false, "disable fontconfig cache support")
	cmd.Flags().BoolVar(&req.NoUdev, "no-udev", false, "disable udev support")
	cmd.Flags().BoolVar(&req.NoEvdev, "no-evdev", false, "disable evdev support")
	cmd.Flags().BoolVar(&req.NoDisplay, "no-display", false, "disable display support")
	cmd.Flags().StringVar(&req.Cache, "cache", "", "cache location (`tmpfs` or `home`); default tmpfs")
	cmd.Flags().BoolVar(&req.Downloads, "downloads", false, "bind the host Downloads directory")
	cmd.Flags().BoolVar(&req.Print, "print", false, "print the bwrap command instead of running it")
	cmd.MarkFlagsMutuallyExclusive("home", "home-tmpfs", "no-home")
	cmd.MarkFlagsMutuallyExclusive("no-dbus", "no-session-dbus")
	_ = cmd.MarkFlagDirname("home")
	return cmd
}
