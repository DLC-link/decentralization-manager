// The Splice localnet's participants, as the Vite dev server proxies them.
// vite.config.ts maps each path to that participant's JSON Ledger API.

export const LOCALNET_PARTICIPANTS = [
  { name: 'App provider', apiUrl: '/participants/app-provider' },
  { name: 'App user', apiUrl: '/participants/app-user' },
  { name: 'SV', apiUrl: '/participants/sv' },
]

// The localnet's `ledger-api-user` token: HS256 with the secret "unsafe". It
// is already public in integration-tests/env.sh and works only on a localnet.
export const LOCALNET_TOKEN =
  'eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k'
