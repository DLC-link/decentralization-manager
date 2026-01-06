import {
  Accordion,
  AccordionSummary,
  AccordionDetails,
  Typography,
  Box,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import type { NodeConfig } from "../types";

const accordionSx = {
  borderRadius: 2,
  mb: 2,
  "&:first-of-type": { borderRadius: 2 },
  "&:last-of-type": { borderRadius: 2 },
  overflow: "hidden",
};

interface NodeConfigAccordionProps {
  config: NodeConfig;
}

export const NodeConfigAccordion = ({ config }: NodeConfigAccordionProps) => {
  return (
    <Accordion defaultExpanded sx={accordionSx}>
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "8px 8px 0 0" }}
      >
        <Typography variant="h6">Node Configuration</Typography>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        <Box>
          <Typography>
            <strong>Node ID:</strong> {config.node.node_id}
          </Typography>
          <Typography>
            <strong>Admin API:</strong> {config.canton.admin_api_host}:
            {config.canton.admin_api_port}
          </Typography>
          <Typography>
            <strong>Ledger API:</strong> {config.canton.ledger_api_host}:
            {config.canton.ledger_api_port}
          </Typography>
          <Typography>
            <strong>Synchronizer:</strong> {config.canton.synchronizer}
          </Typography>
        </Box>
      </AccordionDetails>
    </Accordion>
  );
}
