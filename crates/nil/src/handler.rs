use crate::{convert, Result, StateSnapshot};
use ide::FileRange;
use lsp_types::{
    CompletionParams, CompletionResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover,
    HoverParams, Location, PrepareRenameResponse, ReferenceParams, RenameParams, SelectionRange,
    SelectionRangeParams, SemanticTokens, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult, TextDocumentPositionParams, WorkspaceEdit,
};
use text_size::TextRange;

pub(crate) fn goto_definition(
    snap: StateSnapshot,
    params: GotoDefinitionParams,
) -> Result<Option<GotoDefinitionResponse>> {
    let (_, fpos) = convert::from_file_pos(&snap.vfs(), &params.text_document_position_params)?;
    let targets = match snap.analysis.goto_definition(fpos)? {
        None => return Ok(None),
        Some(targets) => targets,
    };
    let vfs = snap.vfs();
    let targets = targets
        .into_iter()
        .map(|target| {
            convert::to_location(&vfs, FileRange::new(target.file_id, target.focus_range))
        })
        .collect::<Vec<_>>();
    Ok(Some(GotoDefinitionResponse::Array(targets)))
}

pub(crate) fn references(
    snap: StateSnapshot,
    params: ReferenceParams,
) -> Result<Option<Vec<Location>>> {
    let (_, fpos) = convert::from_file_pos(&snap.vfs(), &params.text_document_position)?;
    let refs = match snap.analysis.references(fpos)? {
        None => return Ok(None),
        Some(refs) => refs,
    };
    let vfs = snap.vfs();
    let locs = refs
        .into_iter()
        .map(|frange| convert::to_location(&vfs, frange))
        .collect::<Vec<_>>();
    Ok(Some(locs))
}

pub(crate) fn completion(
    snap: StateSnapshot,
    params: CompletionParams,
) -> Result<Option<CompletionResponse>> {
    let (line_map, fpos) = convert::from_file_pos(&snap.vfs(), &params.text_document_position)?;
    let items = match snap.analysis.completions(fpos)? {
        None => return Ok(None),
        Some(items) => items,
    };
    let items = items
        .into_iter()
        .map(|item| convert::to_completion_item(&line_map, item))
        .collect::<Vec<_>>();
    Ok(Some(CompletionResponse::Array(items)))
}

pub(crate) fn selection_range(
    snap: StateSnapshot,
    params: SelectionRangeParams,
) -> Result<Option<Vec<SelectionRange>>> {
    let file = convert::from_file(&snap.vfs(), &params.text_document)?;
    let line_map = snap.vfs().line_map_for_file(file);
    let ret = params
        .positions
        .iter()
        .map(|&pos| {
            let pos = convert::from_pos(&line_map, pos)?;
            let frange = FileRange::new(file, TextRange::empty(pos));

            let mut ranges = snap.analysis.expand_selection(frange)?.unwrap_or_default();
            if ranges.is_empty() {
                ranges.push(TextRange::empty(pos));
            }

            let mut ret = SelectionRange {
                range: convert::to_range(&line_map, *ranges.last().unwrap()),
                parent: None,
            };
            for &r in ranges.iter().rev().skip(1) {
                ret = SelectionRange {
                    range: convert::to_range(&line_map, r),
                    parent: Some(ret.into()),
                };
            }

            Ok(ret)
        })
        .collect::<Result<Vec<_>>>();
    ret.map(Some)
}

pub(crate) fn prepare_rename(
    snap: StateSnapshot,
    params: TextDocumentPositionParams,
) -> Result<Option<PrepareRenameResponse>> {
    let (line_map, fpos) = convert::from_file_pos(&snap.vfs(), &params)?;
    let (range, text) = snap
        .analysis
        .prepare_rename(fpos)?
        .map_err(convert::to_rename_error)?;
    let resp = convert::to_prepare_rename_response(&line_map, range, text.into());
    Ok(Some(resp))
}

pub(crate) fn rename(snap: StateSnapshot, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
    let (_, fpos) = convert::from_file_pos(&snap.vfs(), &params.text_document_position)?;
    let ws_edit = snap
        .analysis
        .rename(fpos, &params.new_name)?
        .map_err(convert::to_rename_error)?;
    let resp = convert::to_workspace_edit(&snap.vfs(), ws_edit);
    Ok(Some(resp))
}

pub(crate) fn semantic_token_full(
    snap: StateSnapshot,
    params: SemanticTokensParams,
) -> Result<Option<SemanticTokensResult>> {
    let file = convert::from_file(&snap.vfs(), &params.text_document)?;
    let line_map = snap.vfs().line_map_for_file(file);
    let hls = snap.analysis.syntax_highlight(file, None)?;
    let toks = convert::to_semantic_tokens(&line_map, &hls);
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: toks,
    })))
}

pub(crate) fn semantic_token_range(
    snap: StateSnapshot,
    params: SemanticTokensRangeParams,
) -> Result<Option<SemanticTokensRangeResult>> {
    let file = convert::from_file(&snap.vfs(), &params.text_document)?;
    let (line_map, range) = convert::from_range(&snap.vfs(), file, params.range)?;
    let hls = snap.analysis.syntax_highlight(file, Some(range))?;
    let toks = convert::to_semantic_tokens(&line_map, &hls);
    Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
        result_id: None,
        data: toks,
    })))
}

pub(crate) fn hover(snap: StateSnapshot, params: HoverParams) -> Result<Option<Hover>> {
    let (line_map, fpos) =
        convert::from_file_pos(&snap.vfs(), &params.text_document_position_params)?;
    let ret = snap.analysis.hover(fpos)?;
    Ok(ret.map(|hover| convert::to_hover(&line_map, hover)))
}
