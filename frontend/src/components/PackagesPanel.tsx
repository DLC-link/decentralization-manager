import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import {
  Typography,
  Box,
  Table,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
  Button,
  CircularProgress,
  Tooltip,
} from "@mui/material";
import CloudUploadIcon from "@mui/icons-material/CloudUpload";
import CompareArrowsIcon from "@mui/icons-material/CompareArrows";
import SignalWifiOffIcon from "@mui/icons-material/SignalWifiOff";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import { CopyableText } from "./CopyableText";
import { API_BASE } from "../constants";
import { zebraRow } from "../styles";
import type {
  VettedPackageInfo,
  PeerPackageComparison,
  PeerPackageResult,
} from "../types";

interface PackagesPanelProps {
  onUploadDars?: () => void;
  onDistributeDars?: () => void;
}

export const PackagesPanel = ({
  onUploadDars,
  onDistributeDars,
}: PackagesPanelProps) => {
  const [packages, setPackages] = useState<VettedPackageInfo[]>([]);
  const [loadingPackages, setLoadingPackages] = useState(true);

  useEffect(() => {
    setLoadingPackages(true);
    fetch(`${API_BASE}/packages/vetted`)
      .then((res) => (res.ok ? res.json() : []))
      .then((data: VettedPackageInfo[]) => setPackages(data))
      .catch(() => {})
      .finally(() => setLoadingPackages(false));
  }, []);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const [comparison, setComparison] = useState<PeerPackageComparison | null>(
    null,
  );
  const [comparing, setComparing] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const sorted = useMemo(
    () =>
      [...packages].sort((a, b) => {
        const nameCompare = (a.package_name || "").localeCompare(
          b.package_name || "",
        );
        if (nameCompare !== 0) return nameCompare;
        return (a.package_version || "").localeCompare(
          b.package_version || "",
        );
      }),
    [packages],
  );

  const updateScrollShadows = useCallback(() => {
    const el = scrollRef.current;
    if (el) {
      setCanScrollUp(el.scrollTop > 0);
      setCanScrollDown(el.scrollTop < el.scrollHeight - el.clientHeight - 1);
    }
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      updateScrollShadows();
      el.addEventListener("scroll", updateScrollShadows);
      return () => el.removeEventListener("scroll", updateScrollShadows);
    }
  }, [sorted, comparison, updateScrollShadows]);

  const handleComparePeers = async () => {
    setComparing(true);
    try {
      const res = await fetch(`${API_BASE}/packages/compare-peers`);
      if (res.ok) {
        const data: PeerPackageComparison = await res.json();
        setComparison(data);
      }
    } catch (e) {
      console.error("Failed to compare peer packages:", e);
    } finally {
      setComparing(false);
    }
  };

  // Build comparison lookup: for each peer, map "name:version" → true
  const peerLookups = useMemo(() => {
    if (!comparison) return [];
    return comparison.peers.map((peer) => {
      const lookup = new Set(
        peer.packages.map((p) => `${p.name}:${p.version}`),
      );
      return { peer, lookup };
    });
  }, [comparison]);

  const getPeerStatus = (
    peer: PeerPackageResult,
    lookup: Set<string>,
    name: string,
    version: string,
  ): "match" | "mismatch" | "unreachable" => {
    if (!peer.reachable) return "unreachable";
    return lookup.has(`${name}:${version}`) ? "match" : "mismatch";
  };

  const statusColor = (
    status: "match" | "mismatch" | "unreachable",
    rowIndex: number,
  ): string => {
    switch (status) {
      case "match":
        return rowIndex % 2 === 0
          ? "rgba(76, 175, 80, 0.08)"
          : "rgba(76, 175, 80, 0.15)";
      case "mismatch":
        return rowIndex % 2 === 0
          ? "rgba(244, 67, 54, 0.08)"
          : "rgba(244, 67, 54, 0.15)";
      case "unreachable":
        return rowIndex % 2 === 0 ? "transparent" : "action.hover";
    }
  };

  return (
    <Box sx={{ display: "flex", flexDirection: "column", height: "calc(100vh - 200px)" }}>
      <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", mb: 2, flexShrink: 0 }}>
        <Box>
          <Typography variant="body2" color="text.secondary">
            {loadingPackages
              ? "Loading vetted packages..."
              : `${packages.length} packages vetted on this participant`}
          </Typography>
        </Box>
        <Box sx={{ display: "flex", gap: 1 }}>
          <Button
            variant="outlined"
            size="small"
            startIcon={
              comparing ? (
                <CircularProgress size={16} />
              ) : (
                <CompareArrowsIcon />
              )
            }
            onClick={handleComparePeers}
            disabled={comparing}
          >
            {comparing ? "Checking..." : "Check Peer DARs"}
          </Button>
          {onUploadDars && (
            <Button
              variant="contained"
              size="small"
              color="secondary"
              startIcon={<CloudUploadIcon />}
              onClick={onUploadDars}
            >
              Upload DARs
            </Button>
          )}
          {onDistributeDars && (
            <Button
              variant="contained"
              size="small"
              color="secondary"
              startIcon={<CloudUploadIcon />}
              onClick={onDistributeDars}
            >
              Distribute DARs
            </Button>
          )}
        </Box>
      </Box>

        <Box sx={{ position: "relative", flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
          <Box
            sx={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: 16,
              background:
                "linear-gradient(to bottom, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollUp ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
          <Box
            ref={scrollRef}
            sx={{
              flex: 1,
              minHeight: 0,
              overflowY: "auto",
              overflowX: "auto",
            }}
          >
            {comparison ? (
              /* Comparison table */
              <Table size="small" sx={{ minWidth: 650 }}>
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ py: 1, fontWeight: "bold" }}>
                      Package
                    </TableCell>
                    <TableCell sx={{ py: 1, fontWeight: "bold" }}>
                      Version
                    </TableCell>
                    {peerLookups.map(({ peer }) => (
                      <TableCell
                        key={peer.participant_id}
                        sx={{
                          py: 1,
                          fontWeight: "bold",
                          textAlign: "center",
                          opacity: peer.reachable ? 1 : 0.5,
                        }}
                      >
                        <Box
                          sx={{
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "center",
                            gap: 0.5,
                          }}
                        >
                          {peer.name || peer.participant_id}
                          {!peer.reachable && (
                            <Tooltip title="Unreachable" arrow>
                              <SignalWifiOffIcon
                                sx={{ fontSize: 14, color: "text.disabled" }}
                              />
                            </Tooltip>
                          )}
                        </Box>
                      </TableCell>
                    ))}
                  </TableRow>
                </TableHead>
                <TableBody>
                  {comparison.local_packages
                    .slice()
                    .sort((a, b) => a.name.localeCompare(b.name))
                    .map((pkg, idx) => (
                      <TableRow key={pkg.package_id} sx={zebraRow(idx)}>
                        <TableCell sx={{ py: 0.75 }}>
                          {pkg.name || "-"}
                        </TableCell>
                        <TableCell sx={{ py: 0.75 }}>
                          {pkg.version || "-"}
                        </TableCell>
                        {peerLookups.map(({ peer, lookup }) => {
                          const status = getPeerStatus(
                            peer,
                            lookup,
                            pkg.name,
                            pkg.version,
                          );
                          return (
                            <TableCell
                              key={peer.participant_id}
                              sx={{
                                py: 0.75,
                                textAlign: "center",
                                bgcolor: statusColor(status, idx),
                              }}
                            >
                              {status === "match" && (
                                <CheckCircleIcon
                                  sx={{ fontSize: 16, color: "success.main" }}
                                />
                              )}
                              {status === "mismatch" && (
                                <Tooltip title="Missing or version mismatch" arrow>
                                  <ErrorIcon
                                    sx={{ fontSize: 16, color: "error.main" }}
                                  />
                                </Tooltip>
                              )}
                              {status === "unreachable" && (
                                <Typography
                                  variant="caption"
                                  color="text.disabled"
                                >
                                  -
                                </Typography>
                              )}
                            </TableCell>
                          );
                        })}
                      </TableRow>
                    ))}
                </TableBody>
              </Table>
            ) : (
              /* Default local-only table */
              <Table size="small" sx={{ minWidth: 650 }}>
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ py: 1 }}>Package Name</TableCell>
                    <TableCell sx={{ py: 1 }}>Version</TableCell>
                    <TableCell sx={{ py: 1 }}>Package ID</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {sorted.map((p, idx) => (
                    <TableRow key={p.package_id} sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 1 }}>
                        {p.package_name || "-"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        {p.package_version || "-"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        <CopyableText
                          text={p.package_id}
                          truncate={{ start: 16, end: 16 }}
                          variant="body2"
                        />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Box>
          <Box
            sx={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: 16,
              background:
                "linear-gradient(to top, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollDown ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
        </Box>
    </Box>
  );
};
