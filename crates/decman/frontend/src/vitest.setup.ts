import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom has no layout engine, so `window.scrollTo` is unimplemented and every
// call logs "Not implemented". The paging hooks scroll to the top on a page
// change, so stub it here; a test that asserts on the call installs its own spy.
window.scrollTo = () => {};

// Vitest runs without `globals: true`, so Testing Library never registers its
// own auto-cleanup and rendered trees would pile up across tests.
afterEach(cleanup);
