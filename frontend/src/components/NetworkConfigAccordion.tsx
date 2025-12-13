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
import CircleIcon from "@mui/icons-material/Circle";
import type { NetworkConfig, ParticipantStatus } from "../types";

const accordionSx = {
  borderRadius: 3,
  mb: 2,
  "&:first-of-type": { borderRadius: 3 },
  "&:last-of-type": { borderRadius: 3 },
  overflow: "hidden",
};

interface NetworkConfigAccordionProps {
  config: NetworkConfig;
  participantStatuses?: ParticipantStatus[];
}

export const NetworkConfigAccordion = ({
  config,
  participantStatuses,
}: NetworkConfigAccordionProps) => {
  const getStatus = (id: string) =>
    participantStatuses?.find((s) => s.id === id)?.active;
  return (
    <Accordion sx={accordionSx}>
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "12px 12px 0 0" }}
      >
        <Typography variant="h6">Network Configuration</Typography>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        <Box>
          <Typography>
            <strong>Network:</strong> {config.network.name}
          </Typography>
          <Typography>
            <strong>Coordinator Strategy:</strong>{" "}
            {config.network.coordinator_strategy}
          </Typography>
          <Typography variant="subtitle1" sx={{ mt: 2, mb: 1 }}>
            Participants:
          </Typography>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Status</TableCell>
                <TableCell>ID</TableCell>
                <TableCell>Name</TableCell>
                <TableCell>Role</TableCell>
                <TableCell>Address</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {config.participants.map((p) => {
                const isActive = getStatus(p.id);
                return (
                  <TableRow key={p.id}>
                    <TableCell>
                      <CircleIcon
                        sx={{
                          fontSize: 12,
                          color:
                            isActive === undefined
                              ? "text.disabled"
                              : isActive
                                ? "success.main"
                                : "error.main",
                        }}
                      />
                    </TableCell>
                    <TableCell>{p.id}</TableCell>
                    <TableCell>{p.name}</TableCell>
                    <TableCell>
                      <Chip
                        label={p.role || "attestor"}
                        size="small"
                        color={p.role === "coordinator" ? "primary" : "default"}
                      />
                    </TableCell>
                    <TableCell>
                      {p.address}:{p.port}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      </AccordionDetails>
    </Accordion>
  );
}
