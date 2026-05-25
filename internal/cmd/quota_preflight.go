package cmd

import (
	"fmt"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/events"
	"github.com/steveyegge/gastown/internal/quota"
)

// quotaPreflight returns a polecat.SessionStartOptions.PreflightValidator
// closure that gates a spawn on the keychain token state of the resolved
// account. Returns nil when townRoot is empty (no workspace) so callers can
// unconditionally assign without a guard.
//
// On rejection, a TypeQuotaSpawnDenied event is emitted before the error is
// returned so dashboards subscribed to /api/events surface the denial in real
// time.
//
// accountFlag mirrors `--account` and other selection priorities (see
// config.ResolveAccountConfigDir). session is the tmux session ID the spawn
// targets — passed as event payload metadata; empty when not yet known.
func quotaPreflight(townRoot, accountFlag, session string) func() error {
	if townRoot == "" {
		return nil
	}
	return func() error {
		configDir, handle, err := config.ResolveAccountConfigDir(
			constants.MayorAccountsPath(townRoot), accountFlag)
		if err != nil {
			emitSpawnDenied(handle, session, err.Error())
			return err
		}
		if configDir == "" {
			// No accounts configured — nothing to validate against. The spawn
			// will fail later with a clearer error if the agent actually needs
			// an account.
			return nil
		}
		if verr := quota.ValidateKeychainToken(configDir); verr != nil {
			emitSpawnDenied(handle, session, verr.Error())
			return fmt.Errorf("account %q unusable: %w", handle, verr)
		}
		return nil
	}
}

func emitSpawnDenied(handle, session, reason string) {
	_ = events.LogFeed(events.TypeQuotaSpawnDenied, "gt",
		events.QuotaSpawnDeniedPayload(handle, session, reason))
}
