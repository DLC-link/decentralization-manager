import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import {
  Accordion,
  AccordionSummary,
  AccordionDetails,
  Typography,
  Box,
  Table,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
  Chip,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import { CopyableText } from "./CopyableText";
import type { VettedPackageInfo } from "../types";

const accordionSx = {
  borderRadius: 2,
  mb: 2,
  "&:first-of-type": { borderRadius: 2 },
  "&:last-of-type": { borderRadius: 2 },
  overflow: "hidden",
};

interface VettedPackagesAccordionProps {
  packages: VettedPackageInfo[];
}

export const VettedPackagesAccordion = ({ packages }: VettedPackagesAccordionProps) => {
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const sorted = useMemo(
    () =>
      [...packages].sort((a, b) => {
        const nameCompare = (a.package_name || "").localeCompare(b.package_name || "");
        if (nameCompare !== 0) return nameCompare;
        return (a.package_version || "").localeCompare(b.package_version || "");
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
  }, [sorted, updateScrollShadows]);

  return (
    <Accordion sx={accordionSx}>
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "8px 8px 0 0" }}
      >
        <Typography variant="h6">
          Vetted Packages
          <Chip label={packages.length} size="small" sx={{ ml: 1 }} color="primary" />
        </Typography>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 0 }}>
        <Box sx={{ position: "relative" }}>
          {/* Top shadow */}
          <Box
            sx={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: 16,
              background: "linear-gradient(to bottom, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollUp ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
          {/* Scrollable container */}
          <Box
            ref={scrollRef}
            sx={{
              maxHeight: 360, // ~10 rows
              overflowY: "auto",
              overflowX: "auto",
            }}
          >
            <Table size="small" sx={{ minWidth: 650 }}>
              <TableHead>
                <TableRow>
                  <TableCell sx={{ py: 1 }}>Package Name</TableCell>
                  <TableCell sx={{ py: 1 }}>Version</TableCell>
                  <TableCell sx={{ py: 1 }}>Package ID</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {sorted.map((p) => (
                  <TableRow key={p.package_id}>
                    <TableCell sx={{ py: 1 }}>{p.package_name || "-"}</TableCell>
                    <TableCell sx={{ py: 1 }}>{p.package_version || "-"}</TableCell>
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
          </Box>
          {/* Bottom shadow */}
          <Box
            sx={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: 16,
              background: "linear-gradient(to top, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollDown ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
        </Box>
      </AccordionDetails>
    </Accordion>
  );
};
