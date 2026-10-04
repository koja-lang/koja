//! Folding range provider for the Koja LSP.
//!
//! Maps the regions from [`koja_query::folding`] onto protocol
//! ranges. Spans count lines from 1 and the protocol from 0.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_query::folding::{Region, RegionKind, regions};

use crate::backend::Backend;

impl Backend {
    pub(crate) async fn handle_folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;

        self.with_state(&uri, |state| {
            let ranges = state
                .active_file()
                .map(|file| regions(file).into_iter().map(folding_range).collect())
                .unwrap_or_default();
            Ok(Some(ranges))
        })
        .await
    }
}

fn folding_range(region: Region) -> FoldingRange {
    FoldingRange {
        start_line: region.start_line.saturating_sub(1),
        start_character: None,
        end_line: region.end_line.saturating_sub(1),
        end_character: None,
        kind: Some(match region.kind {
            RegionKind::Block => FoldingRangeKind::Region,
            RegionKind::Comment => FoldingRangeKind::Comment,
        }),
        collapsed_text: None,
    }
}
