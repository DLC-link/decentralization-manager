import { Typography, Box } from "@mui/material";
import { CopyableText } from "./CopyableText";
import type { NodeConfig } from "../types";

interface NodeConfigAccordionProps {
  config: NodeConfig;
}

export const NodeConfigAccordion = ({ config }: NodeConfigAccordionProps) => {
  return (
    <Box>
      <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
        Node
      </Typography>
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 0.5 }}>
        <Typography component="span">
          <strong>Participant ID:</strong>
        </Typography>
        <CopyableText
          text={config.node.participant_id}
          truncate={{ start: 16, end: 8 }}
          variant="body1"
        />
      </Box>
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
  );
};
