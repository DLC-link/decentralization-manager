import { useLayoutEffect, useRef } from "react";
import { Alert, Link } from "@mui/material";
import SystemUpdateAltIcon from "@mui/icons-material/SystemUpdateAlt";
import ReportIcon from "@mui/icons-material/Report";

import { releaseNotesUrl, type UpgradeNotice } from "../versionPolicy";

const HEIGHT_VAR = "--version-banner-height";

/**
 * The banner's current height, or 0px without a banner. Elements pinned to the
 * top of the page add it to their offset, so the banner never covers them.
 */
export const BANNER_OFFSET = `var(${HEIGHT_VAR}, 0px)`;

// Fixed in both themes: the banner must read as yellow or orange regardless of
// mode. Both pairs pass WCAG AA contrast.
const COLORS = {
  update: { bg: "#EAB308", fg: "#1A1714" },
  critical: { bg: "#D63A0F", fg: "#FFFFFF" },
} as const;

interface VersionBannerProps {
  notice: UpgradeNotice;
  /** Hides an update notice. A critical notice ignores it. */
  onDismiss: () => void;
}

export const VersionBanner = ({ notice, onDismiss }: VersionBannerProps) => {
  const critical = notice.kind === "critical";
  const { bg, fg } = COLORS[notice.kind];
  const ref = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const root = document.documentElement.style;
    const publish = () => root.setProperty(HEIGHT_VAR, `${el.offsetHeight}px`);
    publish();
    const observer = new ResizeObserver(publish);
    observer.observe(el);
    return () => {
      observer.disconnect();
      root.removeProperty(HEIGHT_VAR);
    };
  }, []);

  return (
    <Alert
      ref={ref}
      variant="filled"
      icon={critical ? <ReportIcon /> : <SystemUpdateAltIcon />}
      onClose={critical ? undefined : onDismiss}
      data-testid={`version-banner-${notice.kind}`}
      sx={{
        borderRadius: 0,
        px: "var(--content-pad)",
        backgroundColor: bg,
        color: fg,
        "& .MuiAlert-icon, & .MuiAlert-action": { color: fg },
      }}
    >
      {critical ? (
        <>
          <strong>Upgrade required.</strong> This node runs v{notice.current}. The minimum
          supported version is v{notice.minimum}. Upgrade to v{notice.latest}.
        </>
      ) : (
        <>
          <strong>Update available.</strong> Decentralization Manager v{notice.latest} is
          out. This node runs v{notice.current}.
        </>
      )}{" "}
      <Link
        href={releaseNotesUrl(notice.latest)}
        target="_blank"
        rel="noopener noreferrer"
        color="inherit"
        sx={{ fontWeight: 600 }}
      >
        Release notes
      </Link>
    </Alert>
  );
};
