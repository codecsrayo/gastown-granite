//go:build !darwin && !linux

package quota

import (
	"encoding/json"
	"errors"
	"time"
)

var errNotDarwin = errors.New("keychain operations are not supported on this OS")

// KeychainCredential holds a backup of a keychain credential for rollback.
type KeychainCredential struct {
	ServiceName string
	Token       string
}

func KeychainServiceName(_ string) string                                          { return "" }
func ReadKeychainToken(_ string) (string, error)                                   { return "", errNotDarwin }
func WriteKeychainToken(_, _, _ string) error                                      { return errNotDarwin }
func SwapKeychainCredential(_, _ string) (*KeychainCredential, error)              { return nil, errNotDarwin }
func SwapOAuthAccount(_, _ string) (json.RawMessage, error)                        { return nil, errNotDarwin }
func ValidateKeychainToken(_ string) error                                         { return nil }
func InspectKeychainToken(_ string) (time.Time, error)                             { return time.Time{}, nil }
func SyncSwappedTokens(_ map[string]string) int                                    { return 0 }
