import type { DisclosedContract } from "./types";

export const API_BASE = "";
export const ADMIN_ACCESS = import.meta.env.VITE_ADMIN_ACCESS === "true";

/** Zebra stripe sx for table rows — subtle alternating background like Apple lists */
export const zebraRow = (index: number) => ({
  bgcolor: index % 2 === 0 ? "transparent" : "action.hover",
});

// Limits
export const MAX_TOTAL_DEPOSIT = 10000;
export const MIN_DEPOSIT_AMOUNT = 0.001;
export const MIN_WITHDRAWAL_AMOUNT = 0.001;

// Contract identifiers (package_ref:module:entity)
export const TEMPLATE_VAULT_RULES = {
  package_ref: "#bitsafe-vault-v0-rc8",
  module: "BitsafeVault.VaultRules",
  entity: "VaultRules",
};
export const TEMPLATE_ALLOCATION_FACTORY = {
  package_ref: "#utility-registry-app-v0",
  module: "Utility.Registry.App.V0.Service.AllocationFactory",
  entity: "AllocationFactory",
};
export const INTERFACE_FEATURED_APP_RIGHT = {
  package_ref: "#splice-api-featured-app-v1",
  module: "Splice.Api.FeaturedAppRightV1",
  entity: "FeaturedAppRight",
  interface: true,
};
export const TEMPLATE_REGISTRAR_SERVICE = {
  package_ref: "#utility-registry-app-v0",
  module: "Utility.Registry.App.V0.Service.Registrar",
  entity: "RegistrarService",
};

export const DEVNET_VAULT_RULES: DisclosedContract = {
  contract_id:
    "006002aa16790251b09f9332e1d57a1a554ad266cb88cc0d382f82a6afa159d66fca1212202bb3e307bd42ea38da22ba40df796d3febdf761572da0dea03c1388e5250241b",
  blob: "CgMyLjES3gQKRQBgAqoWeQJRsJ+TMuHVehpVStJmy4jMDTgvgqavoVnWb8oSEiArs+MHvULqONoiukDfeW0/6992FXLaDeoDwTiOUlAkGxIUYml0c2FmZS12YXVsdC12MC1yYzgaaApAOWIwYWMyZWFjMDkwMzUxOGViNWY3YjBlMDVlZjc0MWI4MWUyZjNjMTBlZDIyM2QyZTE3YjQzYTJiZmI3MDFhYhIMQml0c2FmZVZhdWx0EgpWYXVsdFJ1bGVzGgpWYXVsdFJ1bGVzIrcBarQBClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKWQpXWlUKUzpRdmF1bHQtcmM3LTI6OjEyMjAzYjMxYWEzZTk1ZTZiOTMwYWMyMjA1ZmIzODlkZjQ3NDkyYzc5YTU1YTI2NjI0YWM0ZDExNjcxMTUyNTBhM2Y3KlNiaXRzYWZlLWFkbWluOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YTJRdmF1bHQtcmM3LTI6OjEyMjAzYjMxYWEzZTk1ZTZiOTMwYWMyMjA1ZmIzODlkZjQ3NDkyYzc5YTU1YTI2NjI0YWM0ZDExNjcxMTUyNTBhM2Y3Od0e1gWkSwYAQioKJgokCAESIDYJLxYXfbDSZI5BeBJuxgb+xndivEAUESG2c2KvLMirEB4=",
};
export const DEVNET_VAULT_PROCESSOR_RULES: DisclosedContract = {
  contract_id:
    "00a130ba02f68b74756e77b19b7d81d635fdc4c5d5453386bcda9b3c9d27f2bb3dca121220ec4bc3ed8f6c6f718924719a3775913ac23f0810db4b9ccf0174f5a5d227f6c6",
  blob: "CgMyLjESgAUKRQChMLoC9ot0dW53sZt9gdY1/cTF1UUzhrzamzydJ/K7PcoSEiDsS8Ptj2xvcYkkcZo3dZE6wj8IENtLnM8BdPWl0if2xhIUYml0c2FmZS12YXVsdC12MC1yYzgaegpAOWIwYWMyZWFjMDkwMzUxOGViNWY3YjBlMDVlZjc0MWI4MWUyZjNjMTBlZDIyM2QyZTE3YjQzYTJiZmI3MDFhYhIMQml0c2FmZVZhdWx0EhNWYXVsdFByb2Nlc3NvclJ1bGVzGhNWYXVsdFByb2Nlc3NvclJ1bGVzIr8BarwBClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKYQpfWl0KWzpZYmFja2VuZC1zaWduYXRvcnktMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EqU2JpdHNhZmUtYWRtaW46OjEyMjA5OTk1MzkzNGQ5ZmUxNjNmZWQwN2RkMzcxZmExMzk4MmIyYjMwNzQ5ZDZkZjU2ZWNkYmEzODVmOGM3OGE4NjdhMlliYWNrZW5kLXNpZ25hdG9yeS0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YTmYiekqpEsGAEIqCiYKJAgBEiCFLaBTKthLXPcx1oR1MdGUrGe3yOYhuoJ3IqLkxQHgdxAe",
};

// Party IDs
export const DEVNET_CBTC_DEC_PARTY =
  "cbtc-network::12202a83c6f4082217c175e29bc53da5f2703ba2675778ab99217a5a881a949203ff";
export const DEVNET_VAULT_BACKEND_SIGNATORY =
  "backend-signatory-0::122099953934d9fe163fed07dd371fa13982b2b30749d6df56ecdba385f8c78a867a";
