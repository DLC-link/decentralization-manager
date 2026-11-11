import { keyframes } from "@mui/material/styles";

const pulse = keyframes({ "50%": { opacity: 0.4 } });

const REDUCED = "@media (prefers-reduced-motion: reduce)";

export { pulse, REDUCED };
