use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use chrono::Local;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub const TEMPLATE_OVER_500K_WITH_HISTORY_DOCX: &str = "부품구매요청_교체이럭_유.docx";
pub const TEMPLATE_OVER_500K_WITHOUT_HISTORY_DOCX: &str = "부품구매요청_교체이력_무.docx";
pub const TEMPLATE_UNDER_EQ_500K_WITH_HISTORY_DOCX: &str = "부품구매_교체이력_유.docx";
pub const TEMPLATE_UNDER_EQ_500K_WITHOUT_HISTORY_DOCX: &str = "부품구매_교체이력_무.docx";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurchaseTemplateKind {
    Over500k,
    UnderEq500k,
}

#[derive(Debug, Clone)]
pub struct PurchaseDecision {
    pub note: String,
    pub template_kind: PurchaseTemplateKind,
}

#[derive(Debug, Clone)]
pub struct DocumentRow {
    pub part_key: String,
    pub part_no: String,
    pub part_name: String,
    pub received_date: String,
    pub used_date_last: String,
    pub used_where: String,
    pub usage_reason: String,
    pub replacement_reason: String,
    pub current_stock_before: f64,
    pub required_stock: Option<f64>,
    pub purchase_qty: f64,
    pub purchase_order_note: String,
    pub issued_qty: String,
    pub replacement_dates: [String; 6],
    pub replacement_qtys: [String; 6],
    pub replacement_hosts: [String; 6],
    pub vendor_name: String,
    pub manufacturer_name: String,
    pub unit: String,
    pub unit_price: String,
    pub part_role: String,
    pub template_kind: PurchaseTemplateKind,
    pub has_replacement_history: bool,
}

#[derive(Debug, Clone)]
pub struct TemplateEntry {
    pub name: String,
    pub is_dir: bool,
    pub compression: CompressionMethod,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TemplatePackage {
    pub entries: Vec<TemplateEntry>,
}

#[derive(Debug, Clone)]
pub struct NamedTemplatePackages {
    pub over_500k_with_history: TemplatePackage,
    pub over_500k_without_history: TemplatePackage,
    pub under_eq_500k_with_history: TemplatePackage,
    pub under_eq_500k_without_history: TemplatePackage,
}

pub fn decide_purchase_v2(
    required_stock: Option<f64>,
    current_stock: f64,
    unit_price: Option<f64>,
) -> PurchaseDecision {
    let Some(req) = required_stock else {
        return PurchaseDecision {
            note: "구매 제외: 필수재고량 데이터 없음".to_string(),
            template_kind: PurchaseTemplateKind::UnderEq500k,
        };
    };

    if req <= 0.0 {
        return PurchaseDecision {
            note: "구매 제외: 필수재고량이 0 이하".to_string(),
            template_kind: PurchaseTemplateKind::UnderEq500k,
        };
    }

    if current_stock >= req {
        return PurchaseDecision {
            note: "구매 제외: 현재고가 필수재고량 이상".to_string(),
            template_kind: PurchaseTemplateKind::UnderEq500k,
        };
    }

    if current_stock > req * 0.3 {
        return PurchaseDecision {
            note: "구매 제외: 현재고가 필수재고량의 30% 초과".to_string(),
            template_kind: PurchaseTemplateKind::UnderEq500k,
        };
    }

    let price = unit_price.unwrap_or(0.0);
    if price >= 500_000.0 {
        PurchaseDecision {
            note: "구매 진행: 과거 단가 50만원 이상 -> 부품 구매 요청 품의".to_string(),
            template_kind: PurchaseTemplateKind::Over500k,
        }
    } else {
        PurchaseDecision {
            note: "구매 진행: 과거 단가 50만원 이하 -> 부품 구매 품의".to_string(),
            template_kind: PurchaseTemplateKind::UnderEq500k,
        }
    }
}

pub fn build_purchase_reason_text(row: &DocumentRow) -> String {
    let req = row.required_stock.unwrap_or(0.0).max(0.0);
    let cur = row.current_stock_before.max(0.0);
    if req > 0.0 {
        format!(
            "해당 부품은 {}부품으로서 필수재고 {:.0}개중, 현재고 {:.0}개로 재고확보를 위한 부품 구매 신청",
            row.part_name, req, cur
        )
    } else {
        format!(
            "해당 부품은 {} 부품으로서 현재고 {:.0}개로 재고확보를 위한 부품 구매 신청",
            row.part_name, cur
        )
    }
}

pub fn template_dir_candidates(service_root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![service_root.join("templates")];
    if let Ok(from_env) = std::env::var("PORT_PROJECT_TEMPLATE_DIR") {
        candidates.insert(0, PathBuf::from(from_env));
    }
    candidates
}

pub fn load_named_templates(template_dir: &Path) -> Result<Option<NamedTemplatePackages>, String> {
    let over_hist_yes = template_dir.join(TEMPLATE_OVER_500K_WITH_HISTORY_DOCX);
    let over_hist_no = template_dir.join(TEMPLATE_OVER_500K_WITHOUT_HISTORY_DOCX);
    let under_hist_yes = template_dir.join(TEMPLATE_UNDER_EQ_500K_WITH_HISTORY_DOCX);
    let under_hist_no = template_dir.join(TEMPLATE_UNDER_EQ_500K_WITHOUT_HISTORY_DOCX);

    if !(over_hist_yes.exists()
        && over_hist_no.exists()
        && under_hist_yes.exists()
        && under_hist_no.exists())
    {
        return Ok(None);
    }

    Ok(Some(NamedTemplatePackages {
        over_500k_with_history: load_template_package(&over_hist_yes)?,
        over_500k_without_history: load_template_package(&over_hist_no)?,
        under_eq_500k_with_history: load_template_package(&under_hist_yes)?,
        under_eq_500k_without_history: load_template_package(&under_hist_no)?,
    }))
}

pub fn select_template_for_row<'a>(
    row: &DocumentRow,
    templates: &'a NamedTemplatePackages,
) -> &'a TemplatePackage {
    match row.template_kind {
        PurchaseTemplateKind::Over500k => {
            if row.has_replacement_history {
                &templates.over_500k_with_history
            } else {
                &templates.over_500k_without_history
            }
        }
        PurchaseTemplateKind::UnderEq500k => {
            if row.has_replacement_history {
                &templates.under_eq_500k_with_history
            } else {
                &templates.under_eq_500k_without_history
            }
        }
    }
}

pub fn render_docx_bytes(
    template_pkg: &TemplatePackage,
    row: &DocumentRow,
    serial: usize,
) -> Result<Vec<u8>, String> {
    let mut buffer = std::io::Cursor::new(Vec::<u8>::new());
    let mut zout = ZipWriter::new(&mut buffer);

    for entry in &template_pkg.entries {
        let options = SimpleFileOptions::default()
            .compression_method(entry.compression)
            .unix_permissions(0o644);

        if entry.is_dir {
            zout.add_directory(entry.name.clone(), options)
                .map_err(|err| err.to_string())?;
            continue;
        }

        if entry.name == "word/document.xml" {
            let xml = String::from_utf8(entry.data.clone()).map_err(|err| err.to_string())?;
            let patched = patch_document_xml_docx(&xml, row, serial);
            zout.start_file(
                entry.name.clone(),
                options.compression_method(CompressionMethod::Deflated),
            )
            .map_err(|err| err.to_string())?;
            zout.write_all(patched.as_bytes())
                .map_err(|err| err.to_string())?;
        } else {
            zout.start_file(entry.name.clone(), options)
                .map_err(|err| err.to_string())?;
            zout.write_all(&entry.data).map_err(|err| err.to_string())?;
        }
    }

    zout.finish().map_err(|err| err.to_string())?;
    Ok(buffer.into_inner())
}

fn load_template_package(template: &Path) -> Result<TemplatePackage, String> {
    let tf = File::open(template).map_err(|err| err.to_string())?;
    let mut zin = ZipArchive::new(tf).map_err(|err| err.to_string())?;
    let mut entries = Vec::with_capacity(zin.len());

    for i in 0..zin.len() {
        let mut entry = zin.by_index(i).map_err(|err| err.to_string())?;
        let name = entry.name().to_string();
        let is_dir = entry.is_dir();
        let compression = entry.compression();
        let mut data = Vec::new();
        if !is_dir {
            entry
                .read_to_end(&mut data)
                .map_err(|err| err.to_string())?;
        }
        entries.push(TemplateEntry {
            name,
            is_dir,
            compression,
            data,
        });
    }

    Ok(TemplatePackage { entries })
}

fn patch_document_xml_docx(xml: &str, row: &DocumentRow, serial: usize) -> String {
    let values = build_docx_values(row, serial);
    let compacted = prune_empty_replacement_rows_docx(xml, &values);
    patch_paragraph_text_runs_docx(&compacted, &values)
}

fn patch_paragraph_text_runs_docx(xml: &str, values: &BTreeMap<&'static str, String>) -> String {
    let ranges = find_tag_ranges_docx(xml, "w:p");
    if ranges.is_empty() {
        return xml.to_string();
    }

    let mut out = String::with_capacity(xml.len() + 1024);
    let mut cursor = 0usize;
    for (p_start, p_end) in ranges {
        out.push_str(&xml[cursor..p_start]);
        out.push_str(&patch_one_paragraph_docx(&xml[p_start..p_end], values));
        cursor = p_end;
    }
    out.push_str(&xml[cursor..]);
    out
}

fn patch_one_paragraph_docx(p_xml: &str, values: &BTreeMap<&'static str, String>) -> String {
    let slots = find_text_slots_docx(p_xml);
    if slots.is_empty() {
        return p_xml.to_string();
    }

    let mut plain = String::new();
    for (s, e) in &slots {
        plain.push_str(&xml_unescape_docx(&p_xml[*s..*e]));
    }

    let replaced = replace_tokens_in_text_docx(&plain, values);
    if replaced == plain {
        return p_xml.to_string();
    }

    let mut out = String::with_capacity(p_xml.len() + 64);
    let mut last = 0usize;
    for (idx, (s, e)) in slots.iter().enumerate() {
        out.push_str(&p_xml[last..*s]);
        if idx == 0 {
            out.push_str(&xml_escape_docx(&replaced));
        }
        last = *e;
    }
    out.push_str(&p_xml[last..]);
    out
}

fn find_text_slots_docx(xml: &str) -> Vec<(usize, usize)> {
    let mut slots = Vec::new();
    let mut cursor = 0usize;
    while let Some(t_start_rel) = xml[cursor..].find("<w:t") {
        let mut t_start = cursor + t_start_rel;
        loop {
            let next = xml[t_start + "<w:t".len()..].chars().next();
            let is_exact = matches!(
                next,
                Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')
            );
            if is_exact {
                break;
            }
            let retry_from = t_start + 1;
            let Some(next_rel) = xml[retry_from..].find("<w:t") else {
                return slots;
            };
            t_start = retry_from + next_rel;
        }

        let Some(gt_rel) = xml[t_start..].find('>') else {
            break;
        };
        let content_start = t_start + gt_rel + 1;
        let Some(end_rel) = xml[content_start..].find("</w:t>") else {
            break;
        };
        let content_end = content_start + end_rel;
        slots.push((content_start, content_end));
        cursor = content_end + "</w:t>".len();
    }
    slots
}

fn replace_tokens_in_text_docx(text: &str, values: &BTreeMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut cursor = 0usize;
    while let Some(start_rel) = text[cursor..].find("{{") {
        let start = cursor + start_rel;
        out.push_str(&text[cursor..start]);
        let body_start = start + 2;
        if let Some(end_rel) = text[body_start..].find("}}") {
            let body_end = body_start + end_rel;
            let key = text[body_start..body_end].trim();
            let value = values
                .get(key)
                .cloned()
                .unwrap_or_else(|| "(직접입력)".to_string());
            out.push_str(&value);
            cursor = body_end + 2;
        } else {
            out.push_str(&text[start..]);
            return out;
        }
    }
    out.push_str(&text[cursor..]);
    replace_legacy_double_paren_tokens_docx(&out, values)
}

fn replace_legacy_double_paren_tokens_docx(
    text: &str,
    values: &BTreeMap<&'static str, String>,
) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut cursor = 0usize;
    while let Some(start_rel) = text[cursor..].find("((") {
        let start = cursor + start_rel;
        out.push_str(&text[cursor..start]);
        let body_start = start + 2;
        if let Some(end_rel) = text[body_start..].find("))") {
            let body_end = body_start + end_rel;
            let key = text[body_start..body_end].trim();
            let value = values
                .get(key)
                .cloned()
                .unwrap_or_else(|| text[start..body_end + 2].to_string());
            out.push_str(&value);
            cursor = body_end + 2;
        } else {
            out.push_str(&text[start..]);
            return out;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

fn prune_empty_replacement_rows_docx(xml: &str, values: &BTreeMap<&'static str, String>) -> String {
    let row_ranges = find_tag_ranges_docx(xml, "w:tr");
    if row_ranges.is_empty() {
        return xml.to_string();
    }

    let token_re = match regex::Regex::new(r"\{\{\s*([^}]+?)\s*\}\}") {
        Ok(v) => v,
        Err(_) => return xml.to_string(),
    };

    let mut out = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    for (start, end) in row_ranges {
        out.push_str(&xml[cursor..start]);
        let row_xml = &xml[start..end];
        let plain = extract_plain_text_docx(row_xml);
        let is_replacement_row =
            plain.contains("{{날짜") || plain.contains("{{호기") || plain.contains("{{교체수량");
        if !is_replacement_row {
            out.push_str(row_xml);
            cursor = end;
            continue;
        }

        let mut keep = false;
        for cap in token_re.captures_iter(&plain) {
            let Some(m) = cap.get(1) else {
                continue;
            };
            let key = m.as_str().trim();
            if let Some(v) = values.get(key) {
                if !v.trim().is_empty() {
                    keep = true;
                    break;
                }
            }
        }

        if keep {
            out.push_str(row_xml);
        }
        cursor = end;
    }
    out.push_str(&xml[cursor..]);
    out
}

fn extract_plain_text_docx(xml: &str) -> String {
    let slots = find_text_slots_docx(xml);
    let mut out = String::new();
    for (s, e) in slots {
        out.push_str(&xml_unescape_docx(&xml[s..e]));
    }
    out
}

fn find_tag_ranges_docx(xml: &str, tag: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let start_pat = format!("<{tag}");
    let end_pat = format!("</{tag}>");
    let mut cursor = 0usize;
    while let Some(start_rel) = xml[cursor..].find(&start_pat) {
        let mut start = cursor + start_rel;
        loop {
            let next = xml[start + start_pat.len()..].chars().next();
            let is_exact = matches!(
                next,
                Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')
            );
            if is_exact {
                break;
            }
            let retry_from = start + 1;
            let Some(next_rel) = xml[retry_from..].find(&start_pat) else {
                return out;
            };
            start = retry_from + next_rel;
        }
        let Some(end_rel) = xml[start..].find(&end_pat) else {
            break;
        };
        let end = start + end_rel + end_pat.len();
        out.push((start, end));
        cursor = end;
    }
    out
}

fn build_docx_values(row: &DocumentRow, serial: usize) -> BTreeMap<&'static str, String> {
    let now = Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let doc_date = now.format("%Y%m%d").to_string();
    let purchase_reason = build_purchase_reason_text(row);
    let vendor = fallback_doc_value(&row.vendor_name);
    let new_vendor = "(직접입력)".to_string();
    let manufacturer = fallback_doc_value(&row.manufacturer_name);
    let unit = fallback_doc_value(&row.unit);
    let target_where = fallback_doc_value(&row.used_where);
    let purchase_qty = format!("{:.0}", row.purchase_qty.max(1.0));
    let purchase_qty_num = row.purchase_qty.max(1.0);
    let unit_price_num = parse_numeric_text(&row.unit_price);
    let unit_price_text = unit_price_num
        .map(|n| format_price_docx(&format!("{:.2}", n)))
        .unwrap_or_else(|| fallback_doc_value(&row.unit_price));
    let supply_amount_text = unit_price_num
        .map(|p| format_price_docx(&format!("{:.2}", p * purchase_qty_num)))
        .unwrap_or_else(|| "(직접입력)".to_string());
    let mut m = BTreeMap::new();
    m.insert("번호", row.part_no.clone());
    m.insert("문서번호", format!("DOC-{}-{:04}", doc_date, serial));
    m.insert("작성일자", today);
    m.insert(
        "제목",
        format!("부품 구매 요청 - {} ({})", row.part_name, row.part_no),
    );
    m.insert("품목", row.part_name.clone());
    m.insert("부품명", row.part_name.clone());
    m.insert("품번", row.part_no.clone());
    m.insert("파트넘버", row.part_no.clone());
    m.insert("장비범주", target_where.clone());
    m.insert("장비", target_where.clone());
    m.insert("장비명", target_where.clone());
    m.insert("현재고", format!("{:.0}", row.current_stock_before));
    m.insert("재고", format!("{:.0}", row.current_stock_before));
    m.insert("구매량", purchase_qty.clone());
    m.insert("구매수량", purchase_qty.clone());
    m.insert("구매 직접입력", purchase_qty.clone());
    m.insert("구매직접입력", purchase_qty.clone());
    m.insert("단위", unit);
    m.insert("단가", unit_price_text.clone());
    m.insert("구매사유", purchase_reason.clone());
    m.insert("사유", purchase_reason.clone());
    m.insert("비고", purchase_reason.clone());
    m.insert("구-거래처", vendor.clone());
    m.insert("구거래처", vendor.clone());
    m.insert("공급업체", new_vendor.clone());
    m.insert("신규 거래업체", new_vendor.clone());
    m.insert("부품제조사", manufacturer);
    m.insert("장착수량 직접입력", row.issued_qty.clone());
    m.insert("부품-원리-및-역할", row.part_role.clone());
    m.insert("부품역할", row.part_role.clone());
    m.insert("구매수량 선정 사유", purchase_reason.clone());
    m.insert("공급액", supply_amount_text.clone());
    m.insert("공급가액", supply_amount_text.clone());
    m.insert("공급가액합계", supply_amount_text.clone());
    m.insert("합계", supply_amount_text.clone());
    m.insert("신규 거래업체 단가", unit_price_text.clone());
    m.insert("신규 거래업체 공급가액", supply_amount_text.clone());
    m.insert("신규 업체 공급가액 공급가액", supply_amount_text.clone());
    m.insert("신규거래업체 공급가총액", supply_amount_text);
    m.insert("구단가", unit_price_text);
    m.insert("담당자 직접입력", "(직접입력)".to_string());
    m.insert("번호 직접입력", "(직접입력)".to_string());
    m.insert("업체명 직접입력", new_vendor);
    m.insert("신규 거래업체 담당자", "(직접입력)".to_string());
    m.insert("신규 거래업체 담당자 번호", "(직접입력)".to_string());
    m.insert("신규 업체 납기기간", "(직접입력)".to_string());
    m.insert("향후 정비사항 직접입력", "(직접입력)".to_string());
    m.insert("향후 정비 예정 사항 직접입력", "(직접입력)".to_string());
    m.insert("수리진행여부", "(직접입력)".to_string());
    m.insert("사용일", row.used_date_last.clone());
    m.insert("입고일", row.received_date.clone());
    m.insert("사용처", row.used_where.clone());
    m.insert("문제점", row.usage_reason.clone());
    m.insert("교체사유", row.replacement_reason.clone());
    m.insert("총 교체수량", row.issued_qty.clone());
    m.insert(
        "교체내역 유무",
        if row.has_replacement_history {
            "유".to_string()
        } else {
            "무".to_string()
        },
    );
    m.insert("파트키", row.part_key.clone());
    for idx in 0..6 {
        m.insert(
            Box::leak(format!("날짜{}", idx + 1).into_boxed_str()),
            row.replacement_dates[idx].clone(),
        );
        m.insert(
            Box::leak(format!("호기{}", idx + 1).into_boxed_str()),
            row.replacement_hosts[idx].clone(),
        );
        m.insert(
            Box::leak(format!("교체수량{}", idx + 1).into_boxed_str()),
            row.replacement_qtys[idx].clone(),
        );
    }
    m
}

fn fallback_doc_value(v: &str) -> String {
    if is_missing_doc_value(v) {
        "(직접입력)".to_string()
    } else {
        v.to_string()
    }
}

fn is_missing_doc_value(v: &str) -> bool {
    let t = v.trim();
    t.is_empty() || matches!(t, "기록없음" | "출고기록없음" | "입고기록없음")
}

fn parse_numeric_text(raw: &str) -> Option<f64> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "." {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn format_price_docx(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "(직접입력)".to_string();
    }
    let negative = trimmed.starts_with('-');
    let unsigned = if negative { &trimmed[1..] } else { trimmed };
    let mut parts = unsigned.splitn(2, '.');
    let int_part = parts.next().unwrap_or_default();
    let frac_part = parts.next();
    if !int_part.chars().all(|c| c.is_ascii_digit()) {
        return input.to_string();
    }

    let mut grouped_rev = String::with_capacity(int_part.len() + (int_part.len() / 3));
    for (i, ch) in int_part.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped_rev.push(',');
        }
        grouped_rev.push(ch);
    }
    let mut grouped: String = grouped_rev.chars().rev().collect();

    if let Some(frac) = frac_part {
        let frac_trimmed = frac.trim_end_matches('0');
        if !frac_trimmed.is_empty() {
            grouped.push('.');
            grouped.push_str(frac_trimmed);
        }
    }

    if negative {
        format!("-{}", grouped)
    } else {
        grouped
    }
}

fn xml_escape_docx(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape_docx(v: &str) -> String {
    v.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
