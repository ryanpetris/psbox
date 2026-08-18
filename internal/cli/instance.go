package cli

// Instance list and stop commands.

import (
	"fmt"

	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/units"
)

func newInstanceCommand(svc *units.Service) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "instance",
		Short: "List and stop instances",
	}

	var quiet bool
	list := &cobra.Command{
		Use:   "list",
		Short: "List instances",
		Args:  noArgs("list"),
		RunE: func(cmd *cobra.Command, args []string) error {
			return svc.List(cmd.Context(), quiet, cmd.OutOrStdout())
		},
	}
	list.Flags().BoolVarP(&quiet, "quiet", "q", false, "list identities only")
	cmd.AddCommand(list)

	var all bool
	stop := &cobra.Command{
		Use:               "stop",
		Short:             "Stop an instance",
		Args:              cobra.MaximumNArgs(1),
		ValidArgsFunction: completeInstanceNames(svc),
		RunE: func(cmd *cobra.Command, args []string) error {
			if all {
				if len(args) > 0 {
					return fmt.Errorf("stop --all takes no instance name")
				}
				return svc.StopAll(cmd.Context(), cmd.ErrOrStderr())
			}
			if len(args) != 1 {
				return fmt.Errorf("stop requires an instance name or --all")
			}
			return svc.Stop(cmd.Context(), args[0], cmd.ErrOrStderr())
		},
	}
	stop.Flags().BoolVar(&all, "all", false, "stop every instantiated sandbox")
	cmd.AddCommand(stop)
	return cmd
}

func completeInstanceNames(svc *units.Service) func(*cobra.Command, []string, string) ([]string, cobra.ShellCompDirective) {
	return func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		if len(args) > 0 {
			return nil, cobra.ShellCompDirectiveNoFileComp
		}
		names, err := svc.Identities(cmd.Context())
		if err != nil {
			return nil, cobra.ShellCompDirectiveError
		}
		return names, cobra.ShellCompDirectiveNoFileComp
	}
}
