// Rows per page for every paginated list. Generated from the Rust
// `common::api::PAGE_SIZE` so the wire page size and the UI page size are the
// same number; re-exported here because this is where UI constants are looked
// for.
export { PAGE_SIZE } from "./types.generated";

export const API_BASE = "";
export const ADMIN_ACCESS = import.meta.env.VITE_ADMIN_ACCESS === "true";
/// When true (default), show the full BitSafe wordmark everywhere.
/// When false, show only the "B" mark + "Decentralization Manager" as the
/// app name, with a small "Powered by BitSafe" footer in the sidebar.
export const BITSAFE_BRANDING =
  (import.meta.env.VITE_BITSAFE_BRANDING ?? "true") === "true";

/** Zebra stripe sx for table rows — subtle alternating background like Apple lists */
export const zebraRow = (index: number) => ({
  bgcolor: index % 2 === 0 ? "transparent" : "action.hover",
});

// Contract identifiers (package_ref:module:entity)
export const TEMPLATE_ALLOCATION_FACTORY = {
  package_ref: "#utility-registry-app-v0",
  module: "Utility.Registry.App.V0.Service.AllocationFactory",
  entity: "AllocationFactory",
};
export const TEMPLATE_REGISTRAR_SERVICE = {
  package_ref: "#utility-registry-app-v0",
  module: "Utility.Registry.App.V0.Service.Registrar",
  entity: "RegistrarService",
};
