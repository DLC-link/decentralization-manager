import type { Dispatch, SetStateAction } from "react";
import {
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  List,
  ListItemButton,
  ListItemText,
  TextField,
  Typography,
} from "@mui/material";

interface PartyFilterPickerProps {
  /** The parties with entries and their entry counts, most first. */
  partyOptions: [string, number][];
  /** The selected parties. Empty means every party. */
  partyFilter: string[];
  setPartyFilter: Dispatch<SetStateAction<string[]>>;
  // The dialog state lives in NotificationsView, which shows this picker in
  // two places, so an open dialog stays open when the feed fills in.
  partyPickerOpen: boolean;
  setPartyPickerOpen: (open: boolean) => void;
  partySearch: string;
  setPartySearch: (search: string) => void;
}

/** The party filter button and its search-and-select dialog. */
export const PartyFilterPicker = ({
  partyOptions,
  partyFilter,
  setPartyFilter,
  partyPickerOpen,
  setPartyPickerOpen,
  partySearch,
  setPartySearch,
}: PartyFilterPickerProps) => {
  // At most this many rows are mounted at once. A deployment can hold tens of
  // thousands of parties, and mounting a row each would stall the dialog.
  const PARTY_ROWS_SHOWN = 200;
  const searchLower = partySearch.trim().toLowerCase();
  const matchingParties = partyOptions.filter(
    ([partyId]) => !searchLower || partyId.toLowerCase().includes(searchLower),
  );
  const shownParties = matchingParties.slice(0, PARTY_ROWS_SHOWN);

  return (
    <>
      <Button
        size="small"
        variant="outlined"
        onClick={() => setPartyPickerOpen(true)}
        sx={{
          fontFamily: "var(--font-mono)",
          fontSize: 12,
          textTransform: "none",
          whiteSpace: "nowrap",
        }}
      >
        {partyFilter.length === 0
          ? "All parties"
          : `${partyFilter.length} ${partyFilter.length === 1 ? "party" : "parties"}`}
      </Button>

      <Dialog
        open={partyPickerOpen}
        onClose={() => setPartyPickerOpen(false)}
        fullWidth
        maxWidth="sm"
      >
        <DialogTitle sx={{ pb: 1 }}>Filter by dec party</DialogTitle>
        <DialogContent dividers sx={{ pt: 1 }}>
          <TextField
            size="small"
            fullWidth
            autoFocus
            placeholder="Search parties…"
            value={partySearch}
            onChange={(e) => setPartySearch(e.target.value)}
            sx={{ mb: 1 }}
          />
          <Box
            sx={{
              display: "flex",
              gap: 1,
              alignItems: "center",
              mb: 1,
              flexWrap: "wrap",
            }}
          >
            <Button
              size="small"
              onClick={() =>
                setPartyFilter(matchingParties.map(([partyId]) => partyId))
              }
              disabled={matchingParties.length === 0}
            >
              Select all{searchLower ? " matching" : ""}
            </Button>
            <Button
              size="small"
              onClick={() => setPartyFilter([])}
              disabled={partyFilter.length === 0}
            >
              Deselect all
            </Button>
            <Box sx={{ flex: 1 }} />
            <Typography variant="caption" color="text.secondary">
              {partyFilter.length === 0
                ? "No filter: every party shown"
                : `${partyFilter.length} selected`}
            </Typography>
          </Box>

          <List dense disablePadding sx={{ maxHeight: 360, overflowY: "auto" }}>
            {shownParties.map(([partyId, count]) => {
              const checked = partyFilter.includes(partyId);
              return (
                <ListItemButton
                  key={partyId}
                  onClick={() =>
                    setPartyFilter((current) =>
                      current.includes(partyId)
                        ? current.filter((p) => p !== partyId)
                        : [...current, partyId],
                    )
                  }
                  sx={{ borderRadius: 1 }}
                >
                  <Checkbox
                    edge="start"
                    size="small"
                    checked={checked}
                    tabIndex={-1}
                    disableRipple
                  />
                  <ListItemText
                    primary={
                      <Box
                        component="span"
                        sx={{
                          display: "block",
                          fontFamily: "var(--font-mono)",
                          fontSize: 13,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {partyId.split("::")[0]}
                      </Box>
                    }
                    secondary={`${count} ${count === 1 ? "entry" : "entries"}`}
                  />
                </ListItemButton>
              );
            })}
          </List>

          {matchingParties.length > shownParties.length && (
            <Typography
              variant="caption"
              color="text.secondary"
              sx={{ display: "block", mt: 1 }}
            >
              Showing {shownParties.length} of {matchingParties.length}. Narrow
              the search to reach the rest.
            </Typography>
          )}
          {matchingParties.length === 0 && (
            <Typography
              variant="body2"
              color="text.secondary"
              sx={{ py: 2, textAlign: "center" }}
            >
              No party matches that search.
            </Typography>
          )}
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setPartyPickerOpen(false)}>Done</Button>
        </DialogActions>
      </Dialog>
    </>
  );
};
