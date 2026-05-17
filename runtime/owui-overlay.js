/*
 * CDXVI / 한진 - Open WebUI overlay (v16)
 *
 *  1) Korean tool-name translation (operationId -> 한국어 표시명).
 *     Also sanitizes leaked Gemma tool-call tokens (<|tool_call|>, <|"|>,
 *     etc.) that occasionally bleed into assistant text when the model
 *     mixes prose with structured tool calls in one turn.
 *     v11 also strips reasoning-block leaks: when OWUI's <think> parser
 *     fails to capture a stream cleanly, the close tag (</think>) plus
 *     part of the reasoning text leaks into the visible message. We
 *     detect orphan </think> | </thinking> | </reasoning> close tags
 *     (no matching open in the same text node) and erase from the start
 *     of the node up through that tag.
 *     v15 catches the *plain-prose reasoning leak in body* case:
 *     model emits well-formed `<think>...</think>` but ALSO writes
 *     reasoning-style Korean prose ("검색 결과로 ...를 분석했습니다.",
 *     "이 내용을 바탕으로 ...를 호출하여 ...") AFTER `</think>` as the
 *     first paragraphs of the final answer body. v15 detects the
 *     opener idioms at the top of the body and strips all contiguous
 *     plain-prose paragraphs until the first markdown body marker
 *     (table, link, heading, bold).
 *     v14 catches the *variant open tag* case: model emits `<thought`,
 *     `<thoughts>`, `<thinking>`, `<reasoning>` instead of the strict
 *     `<think>` that OWUI's parser recognizes — the whole reasoning
 *     block leaks into visible body text. v14 detects the open variant
 *     and strips from there through a matching close tag (if any) or
 *     through the end of the text node.
 *     v13 fixes a coverage gap: previously scanTextNodes' acceptNode
 *     only let through nodes containing tool-call control tokens, so
 *     reasoning-leak nodes (e.g. text containing only </think>) never
 *     reached sanitizeText. v13 extends acceptNode to also accept any
 *     node carrying a reasoning-leak signal, and expands the preamble
 *     marker set to cover tool-status labels ("자료 확인 완료",
 *     "PDF 보고서 작성 완료", "결과 보기").
 *     v12 adds a PREAMBLE heuristic for leaks with NO tags at all: when
 *     a message starts with "N초 동안 생각함" or "자료 확인 중" as plain
 *     text followed by English self-talk lines (Plan:, "User wants",
 *     "I should/will"), we cut up to the first Korean answer line.
 *     Gated by (a) marker within first 200 chars AND (b) at least one
 *     English-only line to minimize FP.
 *  2) Thinking toggle: force ON ("Thinking Mode") at boot and on every
 *     new-chat navigation. Drives the OWUI inline toggle directly via its
 *     aria-label values ("Thinking Mode" / "Instant Mode").
 *  3) Agentic Mode chip — single instance, only placed inside the main
 *     chat input bar (next to button#send-message-button). Chips that
 *     somehow land elsewhere (settings forms, admin modals, workspace,
 *     etc.) are removed on sight.
 *  4) Voice / microphone / native agentic indicators hidden via CSS.
 *
 * v8 -> v9: the chat send button is uniquely identified by
 *   `button#send-message-button` in OWUI 0.9.x. Previous versions used a
 *   broad `button[type="submit"]` selector which matched the Save button
 *   of every form (Settings, Admin, Workspace, modals) and scattered the
 *   Agentic chip across the UI. We pin to the unique id and actively
 *   remove stray chips.
 */
(function () {
  const NEW_CHAT_PATHS = ['/', '/c', '/c/'];
  const CHAT_SEND_SELECTOR = 'button#send-message-button';
  let lastPath = location.pathname;
  let cachedToolGroups = null;
  let popoverEl = null;
  let popoverAnchor = null;
  let thinkingNoticeEl = null;
  let thinkingNoticeTimer = null;
  let thinkingRateLockedUntil = 0;
  const THINKING_RATE_WINDOW_MS = 5000;
  const THINKING_RATE_LOCK_MS = 5000;
  const THINKING_RATE_MAX_CHANGES = 4;
  const thinkingChangeClicks = [];
  const THINKING_MODE_STORAGE_KEY = 'cdxvi-thinking-mode-choice';
  let bootDefaultThinkingApplied = false;
  let lastThinkingButton = null;
  let lastThinkingPlacementAnchor = null;
  let lastThinkingPlacementParent = null;

  function getStoredThinkingMode() {
    try {
      const mode = sessionStorage.getItem(THINKING_MODE_STORAGE_KEY);
      return mode === 'instant' || mode === 'thinking' ? mode : null;
    } catch (_e) {
      return null;
    }
  }

  function setStoredThinkingMode(mode) {
    if (mode !== 'instant' && mode !== 'thinking') return;
    try { sessionStorage.setItem(THINKING_MODE_STORAGE_KEY, mode); } catch (_e) {}
  }

  let lastUserActivityAt = 0;
  const TYPING_QUIET_MS = 700;
  const isUserTyping = () => (Date.now() - lastUserActivityAt) < TYPING_QUIET_MS;
  ['keydown', 'input', 'compositionstart', 'compositionupdate', 'compositionend'].forEach((ev) => {
    document.addEventListener(ev, () => { lastUserActivityAt = Date.now(); }, true);
  });

  // -- Korean tool name mapping ------------------------------------------
  const TOOL_TRANSLATIONS = {
    list_shortage_items: '재고 부족 품목 조회',
    list_inventory_items: '재고 목록 조회',
    export_inventory_report: '재고 보고서 파일 생성',
    get_item_document_context: '품목 근거 정보 조회',
    export_single_item_document: '단일 품목 문서 내보내기',
    approve_and_generate_item_document: '품의서 승인·생성',
    generate_purchase_document_package: '구매 품의서 패키지 생성',
    create_document: '문서 생성',
    fill_document: '문서 필드 채우기',
    export_document: '문서 내보내기',
    render_markdown_pdf: 'Markdown 보고서 PDF 변환',
    render_chat_docx: '채팅 기록 Word 내보내기',
    render_chat_xlsx: '채팅 기록 Excel 내보내기',
    search_documents_by_rank: '내부 자료 검색',
    fetch_web_page: '웹 페이지 본문 가져오기',
    '음성 모드 사용': '메시지 보내기',
  };
  const TOOL_KEYS = Object.keys(TOOL_TRANSLATIONS).sort((a, b) => b.length - a.length);

  // -- Gemma tool-call token leak sanitizer ------------------------------
  // When the model produces both text and a structured tool call in the
  // same turn, vLLM's Gemma parser sometimes fails to extract the tool
  // call and lets its delimiter tokens (<|tool_call|>, <|"|>, call:fn{...})
  // through as visible text. We strip those tokens here so the user sees
  // clean prose. The actual tool call still won't execute — system prompt
  // rules are the primary mitigation; this is the display safety net.
  const LEAK_REGEXES = [
    // Open WebUI / Gemma tool-call control tokens in any variant
    /<\|?\/?tool_call\|?>/g,
    // Quote delimiter token Gemma emits inside tool args
    /<\|"\|>/g,
    /<\|\\"\|>/g,
    // Stray leftover bracket sequences that often accompany the above
    /\{?\s*<\|\/?\s*\|\>\s*\}?/g,
  ];
  // Also strip `call:<known_tool>{...}` blocks (best-effort; uses TOOL_KEYS
  // so we don't accidentally strip user-written text that happens to look
  // like a function name).
  const LEAK_CALL_REGEX = new RegExp(
    'call:(?:' + TOOL_KEYS.map((k) => k.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|') + ')\\s*\\{[^}]*\\}?',
    'g'
  );

  // -- v12: Reasoning-preamble leak (no close tag) -----------------------
  // Sometimes the model emits reasoning as raw text with no <think> tags
  // at all (typically opens with OWUI's own thinking label "N초 동안 생각함"
  // appearing as plain text, then runs into English self-talk). v11's
  // orphan-close-tag detector can't see this. We instead detect a clear
  // PREAMBLE pattern at the very start of an assistant message and cut
  // up to the first non-reasoning Korean answer line.
  //
  // Safety gates: only triggers when (a) the leak marker is within the
  // first 200 chars of the node, AND (b) at least one English self-talk
  // line follows. False-positive risk: a user asking literally about the
  // phrase "5초 동안 생각함" — extremely rare and the offer-line UX still
  // works (their text just gets stripped, they re-ask once).
  // v13: marker list expanded to cover tool-status labels that OWUI
  // shows as plain text when its tool-call UI doesn't bind correctly:
  // "자료 확인 완료", "PDF 보고서 작성 완료", "결과 보기", etc.
  const REASONING_PREAMBLE_HEAD = [
    /^\s*\d+\s*초\s*동안\s*생각함\s*$/m,
    /^\s*자료\s*확인\s*(중|완료)\s*$/m,
    /^\s*(생각\s*중|추론\s*완료|분석\s*중)\s*$/m,
    /^\s*PDF\s*보고서\s*작성\s*완료\s*$/m,
    /^\s*Word\s*문서\s*작성\s*완료\s*$/m,
    /^\s*Excel\s*문서\s*작성\s*완료\s*$/m,
    /^\s*결과\s*보기\s*$/m,
  ];
  const REASONING_LINE = [
    /^\s*(The\s+)?[Uu]ser\s+(wants|is\s+asking|needs|has|asked)\b/,
    /^\s*I\s+(have|will|should|need\s+to|am\s+going\s+to|noticed|am\s+thinking|can|tried|just|already|must)\b/,
    /^\s*(Plan|Steps|Approach|Tasks?|Now|Wait|If\s+it)\s*[:,.]/i,
    /^\s*(Wait\s+for|Organize\s+the|Use\s+[A-Z][a-z]+|Combine|Compile|Then\s+I|Searched|Synthesized|Called|Now\s+I|One\s+detail)\b/,
    /^\s*\d+\s*초\s*동안\s*생각함\s*$/,
    /^\s*자료\s*확인\s*(중|완료)\s*$/,
    /^\s*(PDF|Word|Excel)\s*보고서?\s*(작성)?\s*완료\s*$/,
    /^\s*결과\s*보기\s*$/,
    // bullet/numbered self-talk continuation
    /^\s*[-*•]\s+[A-Z][a-z]/,
    /^\s*\d+\.\s+[A-Z][a-z]/,
  ];
  function stripReasoningPreamble(text) {
    if (!text || text.length > 12000) return text;
    // Quick gate: does the text START with a known preamble marker?
    const head = text.slice(0, 200);
    let hasHeadMarker = false;
    for (const re of REASONING_PREAMBLE_HEAD) {
      if (re.test(head)) { hasHeadMarker = true; break; }
    }
    if (!hasHeadMarker) return text;
    // Now scan lines from the top and find the first one that does NOT
    // match any reasoning pattern AND has substantive Korean content.
    const lines = text.split('\n');
    let cutTo = 0;
    let sawEnglishSelfTalk = false;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const trimmed = line.trim();
      if (!trimmed) { cutTo = i + 1; continue; } // blank line — keep skipping
      // Match any reasoning-line pattern?
      let isReasoning = false;
      for (const re of REASONING_LINE) {
        if (re.test(trimmed)) { isReasoning = true; break; }
      }
      // Also treat continuation lines (indented bullets, "- xxx", numbered)
      // that follow a Plan: line as reasoning.
      if (!isReasoning && cutTo > 0 && /^[\s\-\d\.\*•]/.test(line) && /[A-Za-z]/.test(trimmed)) {
        isReasoning = true;
      }
      if (isReasoning) {
        if (/[A-Za-z]/.test(trimmed) && !/[가-힣]/.test(trimmed)) sawEnglishSelfTalk = true;
        cutTo = i + 1;
        continue;
      }
      // First non-reasoning line — stop here.
      break;
    }
    // Only commit the strip if we actually saw English self-talk (gate b).
    if (!sawEnglishSelfTalk) return text;
    return lines.slice(cutTo).join('\n').replace(/^\s+/, '');
  }

  // Orphan reasoning close tags. Only strip when the close tag has NO
  // matching open tag in the same text node — that's the leak signal.
  // If both tags are present, OWUI's own renderer handles the block.
  // v15: When the model leaks reasoning-trace style prose into the
  // *final answer body* (not inside <think>), we strip the leading
  // paragraphs that match. Triggers only when a paragraph at the very
  // start of the text node opens with a known reasoning idiom
  // ("검색 결과로", "이 내용을 바탕으로", "사용자가 ...를 묻고",
  // "~를 호출하겠습니다", "~를 살펴봅니다" 등). Stops at the first
  // paragraph that does NOT match (typical body content: download
  // link, table row, heading).
  // Allow optional leading quote/bracket character so quoted leaks still match
  const Q = `["“”'\`「]*`;
  const BODY_REASONING_OPENERS = [
    new RegExp(`^${Q}검색\\s*결과로\\s`),
    new RegExp(`^${Q}내부\\s*자료\\s*검색을\\s*통해`),
    new RegExp(`^${Q}이\\s*(내용|데이터|자료|정보)을?를?\\s*바탕으로`),
    new RegExp(`^${Q}정리된\\s*(마크다운|내용|보고서)을?`),
    new RegExp(`^${Q}작성된\\s*(내용|보고서)을?`),
    new RegExp(`^${Q}사용자(의|가|는)\\s.+(을|를|이|가|에요|네요)`),
    new RegExp(`^${Q}이번\\s*질문은`),
    new RegExp(`^${Q}방금\\s*결과를\\s*보니`),
    /^.*(를|을)\s*호출(하겠|할\s*예정|합니다|해|해서)/,
    /^.*(를|을)\s*살펴(보겠|봅니다|보는|볼게요)/,
    /^.*(를|을)\s*확인(하겠|했습니다|합니다|할게요|해야겠어요)\.?$/,
    /^.*(를|을)\s*정리(하겠|했습니다|합니다|할게요|해야겠어요)/,
    /^.*(를|을)\s*확보(했습니다|했어요)/,
    /^.*(를|을)\s*구성(했습니다|했어요)/,
    /^.*(를|을)\s*전달(하여|해서)/,
    /^.*(파일|보고서|문서)을?를?\s*(생성|만들)/,
    /^.*분석(했습니다|하겠습니다|했어요)\.?$/,
    /^.*(필요해서|있어야|위해서?|필요하겠어요|필요하겠네요)\s.+(하겠|호출|살펴|확인|정리|만들|보겠|볼게요)/,
  ];
  function stripBodyReasoningLeak(text) {
    if (!text || text.length > 12000) return text;
    const paragraphs = text.split(/\n\s*\n+/);
    // Step 1: confirm leak by matching first non-empty paragraph against
    // a reasoning idiom. If no match, leave the message untouched.
    let firstIdx = -1;
    for (let i = 0; i < paragraphs.length; i++) {
      if (paragraphs[i].trim()) { firstIdx = i; break; }
    }
    if (firstIdx === -1) return text;
    let isLeak = false;
    for (const re of BODY_REASONING_OPENERS) {
      if (re.test(paragraphs[firstIdx].trim())) { isLeak = true; break; }
    }
    if (!isLeak) return text;
    // Step 2: starting from the leak-confirmed paragraph, also strip any
    // contiguous follow-up paragraph that is plain Korean prose ending in
    // 다./니다. AND contains no markdown body markers. Stop at first
    // paragraph that looks like real body content (table, link, heading,
    // bold-led emphasis).
    let cut = firstIdx + 1;
    for (let i = firstIdx + 1; i < paragraphs.length; i++) {
      const p = paragraphs[i].trim();
      if (!p) { cut = i + 1; continue; }
      const hasMarkdownBody = /^[#\->*|]|^!?\[.+\]\(|^\*\*|\|/.test(p);
      const endsLikeProse = /(다|니다|요)\.\s*$/.test(p);
      if (!hasMarkdownBody && endsLikeProse) { cut = i + 1; continue; }
      break;
    }
    return paragraphs.slice(cut).join('\n\n').replace(/^\s+/, '');
  }

  // v14: Variant OPEN tags (e.g. `<thought`, `<thoughts>`, `<thinking>`,
  // `<reasoning>`, also un-closed `<think ` with stray space) appear in
  // assistant body when the model mis-spells the tag — OWUI only parses
  // strict `<think>...</think>`, so any variant leaks the whole reasoning
  // block as plain text. We detect those open tags at the start of a
  // text node and strip from the tag through the end of the node (or
  // through a matching close tag if present).
  const REASONING_OPEN_VARIANTS = [
    /<thought[s]?\b[^>]*>?/i,
    /<thinking\b[^>]*>?/i,
    /<reasoning\b[^>]*>?/i,
    // `<think ` with literal trailing space — strict <think> is OWUI-valid
    // and handled by its native parser, so we exclude well-formed openers.
    /<think\s+[^>]*$/i,
  ];
  function stripVariantOpenLeak(text) {
    if (!text || text.length > 12000) return text;
    for (const re of REASONING_OPEN_VARIANTS) {
      const m = text.match(re);
      if (!m) continue;
      const openIdx = m.index;
      // If a matching standard close tag exists later in this node, cut
      // from open through close inclusive. Otherwise cut from open to end.
      const rest = text.slice(openIdx);
      const closeMatch = rest.match(/<\/(thought[s]?|thinking|reasoning|think)>/i);
      if (closeMatch) {
        const cutEnd = openIdx + closeMatch.index + closeMatch[0].length;
        text = text.slice(0, openIdx) + text.slice(cutEnd);
      } else {
        text = text.slice(0, openIdx);
      }
      text = text.replace(/\s+$/, '');
    }
    return text;
  }

  const REASONING_CLOSE_TAGS = ['think', 'thinking', 'reasoning'];
  function stripOrphanReasoningClose(text) {
    if (!text || text.length > 8000) return text; // cap to avoid pathological cases
    for (const tag of REASONING_CLOSE_TAGS) {
      const closeIdx = text.toLowerCase().lastIndexOf('</' + tag + '>');
      if (closeIdx === -1) continue;
      const before = text.slice(0, closeIdx);
      const openRe = new RegExp('<' + tag + '(?:\\s[^>]*)?>', 'i');
      if (openRe.test(before)) continue; // properly paired — leave it
      // Orphan close. Cut everything from start of node through the tag.
      const cutEnd = closeIdx + ('</' + tag + '>').length;
      text = text.slice(cutEnd).replace(/^\s+/, '');
    }
    return text;
  }

  function sanitizeText(text) {
    if (!text) return text;
    let out = text;
    // 1a) Reasoning leak (preamble heuristic, v12): no close tag, raw self-talk
    out = stripReasoningPreamble(out);
    // 1b) Reasoning leak (orphan close tag, v11)
    if (out.indexOf('</') !== -1) out = stripOrphanReasoningClose(out);
    // 1c) Reasoning leak (variant OPEN tag, v14): <thought / <thoughts / <thinking / <reasoning
    if (out.indexOf('<') !== -1) out = stripVariantOpenLeak(out);
    // 1d) Reasoning leak as plain prose in body (v15)
    out = stripBodyReasoningLeak(out);
    // 2) Gemma tool-call leak: control tokens
    if (out.indexOf('<|') !== -1 || out.indexOf('call:') !== -1) {
      for (const re of LEAK_REGEXES) out = out.replace(re, '');
      if (out.indexOf('call:') !== -1) out = out.replace(LEAK_CALL_REGEX, '');
    }
    return out;
  }

  function sanitizeNode(node) {
    const t = node.nodeValue;
    if (!t) return;
    const cleaned = sanitizeText(t);
    if (cleaned !== t) node.nodeValue = cleaned;
  }


  function translateNode(node) {
    let t = node.nodeValue;
    if (!t) return;
    // First, strip any leaked tool-call control tokens from the text.
    const cleaned = sanitizeText(t);
    if (cleaned !== t) t = cleaned;
    let changed = cleaned !== node.nodeValue;
    for (const k of TOOL_KEYS) {
      if (t.includes(k)) { t = t.split(k).join(TOOL_TRANSLATIONS[k]); changed = true; }
    }
    if (changed) node.nodeValue = t;
  }

  // v13: nodes containing reasoning-leak signals must also be accepted,
  // otherwise sanitizeText never sees them. Cheap signal set:
  const REASONING_LEAK_SIGNALS = [
    '</think>', '</thinking>', '</reasoning>',
    '<thought', '<thoughts', '<thinking', '<reasoning',
    '검색 결과로', '이 내용을 바탕으로', '정리된 마크다운을',
    '초 동안 생각함', '자료 확인 완료', '자료 확인 중',
    'PDF 보고서 작성 완료', 'Word 문서 작성 완료', 'Excel 문서 작성 완료',
    '결과 보기',
    'User wants', 'user wants', 'I have already', 'I will',
    'I should', 'I tried', 'Plan:', 'Wait for ',
  ];
  function nodeHasReasoningSignal(v) {
    for (const s of REASONING_LEAK_SIGNALS) if (v.indexOf(s) !== -1) return true;
    return false;
  }

  function scanTextNodes(root) {
    if (!root) return;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode(n) {
        const v = n.nodeValue;
        if (!v) return NodeFilter.FILTER_REJECT;
        if (v.indexOf('<|') !== -1 || v.indexOf('call:') !== -1) return NodeFilter.FILTER_ACCEPT;
        if (nodeHasReasoningSignal(v)) return NodeFilter.FILTER_ACCEPT;
        for (const k of TOOL_KEYS) if (v.includes(k)) return NodeFilter.FILTER_ACCEPT;
        return NodeFilter.FILTER_REJECT;
      },
    });
    const batch = [];
    let n;
    while ((n = walker.nextNode())) batch.push(n);
    batch.forEach(translateNode);
  }

  // -- Inline Thinking toggle --------------------------------------------
  function thinkingButtons() {
    return Array.from(document.querySelectorAll('button')).filter((b) => {
      const aria = (b.getAttribute('aria-label') || '').toLowerCase();
      return aria === 'thinking mode' || aria === 'instant mode';
    });
  }
  const isThinkingActive = (btn) =>
    (btn.getAttribute('aria-label') || '').toLowerCase() === 'thinking mode';

  const THINKING_LABEL_BY_ARIA = {
    'thinking mode': '정밀 검토',
    'instant mode': '빠른 응답',
  };
  const THINKING_SEGMENT_OPTIONS = [
    ['instant', '빠른 응답'],
    ['thinking', '정밀 검토'],
  ];

  function thinkingModeKey(btn) {
    const aria = (btn?.getAttribute('aria-label') || '').toLowerCase();
    if (aria === 'thinking mode') return 'thinking';
    if (aria === 'instant mode') return 'instant';
    return null;
  }

  function syncThinkingSegmentVisual(btn, mode = thinkingModeKey(btn)) {
    const track = btn?.querySelector('.cdxvi-thinking-segment-track');
    if (!track || !mode) return;
    track.classList.toggle('cdxvi-thinking-active', mode === 'thinking');
    track.querySelectorAll('[data-cdxvi-thinking-mode]').forEach((option) => {
      option.classList.toggle('cdxvi-active', option.dataset.cdxviThinkingMode === mode);
    });
  }

  function installThinkingSegmentGuard(btn) {
    if (!btn || btn.dataset.cdxviThinkingSegmentGuard === '1') return;
    btn.dataset.cdxviThinkingSegmentGuard = '1';
    btn.addEventListener('click', (event) => {
      const option = event.target.closest?.('[data-cdxvi-thinking-mode]');
      const requestedMode = option?.dataset?.cdxviThinkingMode;
      const currentMode = thinkingModeKey(btn);
      if (!requestedMode || !currentMode) return;
      if (requestedMode === currentMode) {
        event.preventDefault();
        event.stopImmediatePropagation();
        return;
      }
      setStoredThinkingMode(requestedMode);
      syncThinkingSegmentVisual(btn, requestedMode);
      requestAnimationFrame(() => syncThinkingSegmentVisual(btn, requestedMode));
      setTimeout(tagThinkingButtons, 180);
    }, true);
  }

  function translateThinkingButtonText(btn) {
    const mode = thinkingModeKey(btn);
    if (!mode) return;
    lastThinkingButton = btn;
    btn.classList.add('cdxvi-thinking-toggle', 'cdxvi-thinking-segmented');
    btn.setAttribute('title', '응답 모드 선택');
    installThinkingSegmentGuard(btn);

    let track = btn.querySelector('.cdxvi-thinking-segment-track');
    if (!track) {
      btn.textContent = '';
      track = document.createElement('span');
      track.className = 'cdxvi-thinking-segment-track';
      THINKING_SEGMENT_OPTIONS.forEach(([key, label]) => {
        const option = document.createElement('span');
        option.className = 'cdxvi-thinking-segment-option';
        option.dataset.cdxviThinkingMode = key;
        option.textContent = label;
        track.append(option);
      });
      btn.append(track);
    }
    syncThinkingSegmentVisual(btn, mode);
  }

  function tagThinkingButtons() {
    thinkingButtons().forEach((b) => {
      if (!b.classList.contains('cdxvi-thinking-toggle')) b.classList.add('cdxvi-thinking-toggle');
      translateThinkingButtonText(b);
    });
  }

  function applyThinkingMode(mode, maxAttempts = 12, intervalMs = 250) {
    let attempts = 0;
    const tick = () => {
      attempts += 1;
      const btns = thinkingButtons();
      let stillWrong = false;
      btns.forEach((b) => {
        if (thinkingModeKey(b) !== mode) {
          stillWrong = true;
          try { b.click(); } catch (_e) {}
        }
      });
      if ((btns.length === 0 || stillWrong) && attempts < maxAttempts) {
        setTimeout(tick, intervalMs);
      }
    };
    tick();
  }

  function applyInitialThinkingDefault() {
    if (bootDefaultThinkingApplied || getStoredThinkingMode()) return;
    bootDefaultThinkingApplied = true;
    applyThinkingMode('thinking');
  }

  function applyStoredThinkingMode(maxAttempts = 6, intervalMs = 200) {
    const mode = getStoredThinkingMode();
    if (mode) applyThinkingMode(mode, maxAttempts, intervalMs);
  }

  const isNewChatPath = (p) => NEW_CHAT_PATHS.includes(p) || p === '';

  // -- Tool groups + popover (unchanged from v8) -------------------------
  const SERVER_TO_GROUP = {
    'server:document_search': '내부 자료 검색',
    'server:document_generation_tools': '문서 작성·재고 조회',
    'server:markdown_pdf_tools': '보고서 렌더링 (PDF/Word/Excel)',
    'server:web_tools': '웹 페이지 본문',
  };
  const DEFAULT_TOOL_GROUPS = Object.values(SERVER_TO_GROUP);

  async function fetchActiveToolGroups() {
    if (cachedToolGroups) return cachedToolGroups;
    try {
      const r = await fetch('/api/v1/models', { credentials: 'include' });
      if (!r.ok) throw new Error('models api ' + r.status);
      const data = await r.json();
      const models = Array.isArray(data) ? data : data.data || [];
      const preferred = models.find((m) => (m.id || '').includes('cdxvi'))
        || models.find((m) => m.meta && m.meta.toolIds && m.meta.toolIds.length)
        || null;
      if (preferred && preferred.meta && Array.isArray(preferred.meta.toolIds)) {
        const groups = preferred.meta.toolIds.map((id) => SERVER_TO_GROUP[id] || id).filter(Boolean);
        if (groups.length) {
          cachedToolGroups = groups;
          if (popoverEl && popoverAnchor && popoverEl.classList.contains('cdxvi-show')) {
            populatePopover(cachedToolGroups);
            positionPopover(popoverAnchor);
          }
          return groups;
        }
      }
    } catch (_e) {}
    cachedToolGroups = DEFAULT_TOOL_GROUPS;
    return cachedToolGroups;
  }

  function ensurePopoverEl() {
    if (popoverEl && popoverEl.isConnected) return popoverEl;
    popoverEl = document.createElement('div');
    popoverEl.className = 'cdxvi-agentic-popover';
    popoverEl.setAttribute('role', 'dialog');
    popoverEl.addEventListener('click', (e) => e.stopPropagation());
    document.body.appendChild(popoverEl);
    return popoverEl;
  }

  function populatePopover(groups) {
    const pop = ensurePopoverEl();
    pop.innerHTML = '';
    const title = document.createElement('div');
    title.className = 'cdxvi-agentic-popover-title';
    title.textContent = '현재 적용 중인 도구';
    pop.appendChild(title);
    const ul = document.createElement('ul');
    groups.forEach((g) => {
      const li = document.createElement('li');
      li.textContent = g;
      ul.appendChild(li);
    });
    pop.appendChild(ul);
  }

  function positionFloatingPopover(pop, anchorBtn) {
    if (!pop || !anchorBtn) return;
    const margin = 8;
    const gap = 10;
    const anchorRect = anchorBtn.getBoundingClientRect();
    const wasShown = pop.classList.contains('cdxvi-show');
    if (!wasShown) pop.style.visibility = 'hidden';
    pop.classList.add('cdxvi-show');
    const popRect = pop.getBoundingClientRect();

    let top = anchorRect.top - popRect.height - gap;
    let below = false;
    if (top < margin) { top = anchorRect.bottom + gap; below = true; }
    let left = anchorRect.left + anchorRect.width / 2 - popRect.width / 2;
    if (left < margin) left = margin;
    if (left + popRect.width > window.innerWidth - margin) {
      left = window.innerWidth - popRect.width - margin;
    }

    pop.style.top = `${Math.round(top)}px`;
    pop.style.left = `${Math.round(left)}px`;
    pop.classList.toggle('cdxvi-below', below);
    const tailX = anchorRect.left + anchorRect.width / 2 - left;
    pop.style.setProperty('--cdxvi-tail-x', `${Math.round(tailX)}px`);

    if (!wasShown) { pop.classList.remove('cdxvi-show'); pop.style.visibility = ''; }
  }

  function positionPopover(anchorBtn) {
    positionFloatingPopover(popoverEl, anchorBtn);
  }

  function showPopover(anchorBtn) {
    ensurePopoverEl();
    populatePopover(cachedToolGroups || DEFAULT_TOOL_GROUPS);
    positionPopover(anchorBtn);
    popoverEl.classList.add('cdxvi-show');
    popoverAnchor = anchorBtn;
    anchorBtn.setAttribute('aria-expanded', 'true');
    if (!cachedToolGroups) fetchActiveToolGroups();
  }

  function hidePopover() {
    if (popoverEl) popoverEl.classList.remove('cdxvi-show');
    if (popoverAnchor) popoverAnchor.setAttribute('aria-expanded', 'false');
    popoverAnchor = null;
  }

  function togglePopover(anchorBtn) {
    const isOpen = popoverEl && popoverEl.classList.contains('cdxvi-show') && popoverAnchor === anchorBtn;
    if (isOpen) hidePopover();
    else showPopover(anchorBtn);
  }

  // -- Agentic chip: chat-input-only placement ---------------------------
  // The main chat send button has a unique id; we use it as the anchor.
  // Any chip that exists outside this anchor's surrounding flex row is
  // immediately removed.
  function decorateSendFallbackButton(btn) {
    if (!btn) return null;
    btn.dataset.cdxviSendFallback = '1';
    btn.id = 'send-message-button';
    btn.type = 'submit';
    btn.classList.add('cdxvi-send-fallback');
    btn.setAttribute('aria-label', '메시지 보내기');
    btn.setAttribute('title', '메시지 보내기');
    btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" class="size-5"><path fill-rule="evenodd" d="M8 14a.75.75 0 0 1-.75-.75V4.56L4.03 7.78a.75.75 0 0 1-1.06-1.06l4.5-4.5a.75.75 0 0 1 1.06 0l4.5 4.5a.75.75 0 0 1-1.06 1.06L8.75 4.56v8.69A.75.75 0 0 1 8 14Z" clip-rule="evenodd"></path></svg>';
    if (btn.dataset.cdxviSendFallbackGuard !== '1') {
      btn.dataset.cdxviSendFallbackGuard = '1';
      btn.addEventListener('click', (event) => {
        event.stopImmediatePropagation();
      }, true);
      btn.addEventListener('mouseenter', () => setTimeout(positionSendTooltip, 0), true);
      btn.addEventListener('mousemove', () => setTimeout(positionSendTooltip, 0), true);
      btn.addEventListener('focus', () => setTimeout(positionSendTooltip, 0), true);
    }
    return btn;
  }

  function positionSendTooltip() {
    const btn = document.querySelector('.cdxvi-send-fallback');
    if (!btn) return;
    const tooltip = Array.from(document.querySelectorAll('[role="tooltip"], .tooltip, [data-floating-ui-portal] *'))
      .filter((el) => el instanceof HTMLElement)
      .find((el) => (el.textContent || '').trim() === '메시지 보내기' || (el.textContent || '').trim() === '음성 모드 사용');
    if (!tooltip) return;
    const rect = btn.getBoundingClientRect();
    const tipRect = tooltip.getBoundingClientRect();
    const left = Math.round(rect.left + rect.width / 2 - tipRect.width / 2);
    const top = Math.round(rect.top - tipRect.height - 8);
    tooltip.textContent = '메시지 보내기';
    tooltip.style.position = 'fixed';
    tooltip.style.left = `${Math.max(8, left)}px`;
    tooltip.style.top = `${Math.max(8, top)}px`;
    tooltip.style.transform = 'none';
  }

  function findChatSendButton() {
    const send = document.querySelector(CHAT_SEND_SELECTOR);
    if (send) {
      if (send.classList.contains('cdxvi-send-fallback') || send.dataset.cdxviSendFallback === '1') {
        return decorateSendFallbackButton(send);
      }
      return send;
    }
    const fallback = document.querySelector('.cdxvi-send-fallback')
      || Array.from(document.querySelectorAll('button')).find((btn) => {
        const aria = (btn.getAttribute('aria-label') || '').toLowerCase();
        return aria === '음성 모드 사용' || aria.includes('voice mode');
      });
    return fallback ? decorateSendFallbackButton(fallback) : null;
  }

  // The "control row" we care about is the flex row that contains the
  // chat send button. We treat that ancestor as the chip's home.
  function chatChipHomeFor(sendBtn) {
    return sendBtn ? sendBtn.parentElement : null;
  }

  function placeThinkingBySendButton(sendBtn = findChatSendButton()) {
    const home = chatChipHomeFor(sendBtn);
    if (!home || !sendBtn) return;

    const alreadyPlaced = lastThinkingButton
      && lastThinkingButton.parentElement === home
      && lastThinkingButton.nextSibling === sendBtn
      && lastThinkingPlacementAnchor === sendBtn
      && lastThinkingPlacementParent === home;
    if (alreadyPlaced) return;

    const btns = thinkingButtons().filter((btn) => btn.isConnected);
    let target = btns.find((btn) => btn.parentElement === home && btn.nextSibling === sendBtn) || null;
    if (!target) {
      target = btns.find((btn) => {
        const rect = btn.getBoundingClientRect();
        const style = getComputedStyle(btn);
        return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden';
      }) || btns[0] || null;
    }
    if (!target && lastThinkingButton) target = lastThinkingButton;
    if (!target) return;

    translateThinkingButtonText(target);
    if (target.parentElement !== home || target.nextSibling !== sendBtn) {
      home.insertBefore(target, sendBtn);
    }
    lastThinkingPlacementAnchor = sendBtn;
    lastThinkingPlacementParent = home;

    thinkingButtons().forEach((btn) => {
      if (btn !== target && btn.classList.contains('cdxvi-thinking-toggle')) btn.remove();
    });
  }

  function findPrimaryThinkingButton() {
    return thinkingButtons().find((btn) => btn.isConnected) || null;
  }

  function findInputMenuButton() {
    return document.querySelector('#input-menu-button');
  }

  function findAgenticTray() {
    const menu = findInputMenuButton();
    const root = menu?.closest('.flex.items-center.min-w-0');
    if (!root) return null;
    return Array.from(root.children).find((el) => {
      if (!(el instanceof HTMLElement)) return false;
      return el.classList.contains('ml-1') && el.classList.contains('flex') && el.classList.contains('items-center');
    }) || null;
  }

  function cleanupStrayChips() {
    const tray = findAgenticTray();
    const chips = Array.from(document.querySelectorAll('.cdxvi-agentic-button'));
    let keeper = tray ? chips.find((chip) => chip.parentElement === tray) : null;
    if (!keeper) keeper = chips[0] || null;
    chips.forEach((chip) => {
      if (chip !== keeper) chip.remove();
    });
  }

  function placeChatControls(chip, sendBtn) {
    const sendHome = chatChipHomeFor(sendBtn);
    if (!chip || !sendHome || !sendBtn) return;

    const tray = findAgenticTray();
    if (tray) {
      const firstForeign = Array.from(tray.children).find((el) => el !== chip);
      if (chip.parentElement !== tray || chip.nextSibling !== firstForeign) {
        tray.insertBefore(chip, firstForeign || null);
      }
    } else if (chip.parentElement !== sendHome) {
      sendHome.insertBefore(chip, sendBtn);
    }

    placeThinkingBySendButton(sendBtn);
  }

  function createAgenticButton() {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'cdxvi-agentic-button';
    btn.setAttribute('aria-haspopup', 'dialog');
    btn.setAttribute('aria-expanded', 'false');
    btn.setAttribute('title', '클릭하면 현재 적용 도구 그룹이 표시됩니다');
    btn.appendChild(document.createTextNode('Agentic Mode'));
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      togglePopover(btn);
    });
    return btn;
  }

  function ensureAgenticButton() {
    const sendBtn = findChatSendButton();
    if (!sendBtn) {
      // No chat input on this route (e.g., settings page). Make sure no
      // chip survives from a previous chip-injection attempt.
      cleanupStrayChips();
      return null;
    }
    const home = chatChipHomeFor(sendBtn);
    tagThinkingButtons();
    cleanupStrayChips();
    let chip = home.querySelector(':scope > .cdxvi-agentic-button') || document.querySelector('.cdxvi-agentic-button');
    if (!chip) chip = createAgenticButton();
    placeChatControls(chip, sendBtn);
    return chip;
  }

  // -- Outside-click / focus-in / scroll / resize / esc ------------------
  const isInsidePopoverOrButton = (target) => !!(target && (
    target.closest && (
      target.closest('.cdxvi-agentic-button') || target.closest('.cdxvi-agentic-popover')
    )
  ));
  document.addEventListener('mousedown', (e) => {
    if (!popoverEl || !popoverEl.classList.contains('cdxvi-show')) return;
    if (!isInsidePopoverOrButton(e.target)) hidePopover();
  });
  document.addEventListener('focusin', (e) => {
    if (!popoverEl || !popoverEl.classList.contains('cdxvi-show')) return;
    if (!isInsidePopoverOrButton(e.target)) hidePopover();
  });
  window.addEventListener('scroll', () => {
    if (popoverAnchor) positionPopover(popoverAnchor);
    const notice = thinkingNoticeEl;
    const anchor = findPrimaryThinkingButton();
    if (notice && notice.classList.contains('cdxvi-show') && anchor) positionFloatingPopover(notice, anchor);
  }, { passive: true });
  window.addEventListener('resize', () => {
    if (popoverAnchor) positionPopover(popoverAnchor);
    const notice = thinkingNoticeEl;
    const anchor = findPrimaryThinkingButton();
    if (notice && notice.classList.contains('cdxvi-show') && anchor) positionFloatingPopover(notice, anchor);
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && popoverEl && popoverEl.classList.contains('cdxvi-show')) hidePopover();
  });

  // -- Route watching ----------------------------------------------------
  function watchRoute() {
    const _push = history.pushState;
    const _replace = history.replaceState;
    history.pushState = function () {
      const r = _push.apply(this, arguments);
      window.dispatchEvent(new Event('cdxvi:locationchange'));
      return r;
    };
    history.replaceState = function () {
      const r = _replace.apply(this, arguments);
      window.dispatchEvent(new Event('cdxvi:locationchange'));
      return r;
    };
    window.addEventListener('popstate', () => window.dispatchEvent(new Event('cdxvi:locationchange')));
    window.addEventListener('cdxvi:locationchange', () => {
      const newPath = location.pathname;
      if (newPath !== lastPath) {
        lastPath = newPath;
        if (isNewChatPath(newPath)) {
          applyInitialThinkingDefault();
          applyStoredThinkingMode(4, 200);
        }
        setTimeout(ensureAgenticButton, 300);
      }
    });
  }

  // -- Mutation observer -------------------------------------------------
  let observerTimer = null;

  function runExpensiveSync() {
    ensureAgenticButton();
    tagThinkingButtons();
    if (isUserTyping()) { scheduleExpensiveSync(); return; }
  }

  function scheduleExpensiveSync() {
    if (observerTimer) return;
    const delay = isUserTyping() ? TYPING_QUIET_MS + 50 : 500;
    observerTimer = setTimeout(() => {
      observerTimer = null;
      try { runExpensiveSync(); } catch (_e) {}
    }, delay);
  }

  let fastControlSyncTimer = null;
  function scheduleFastControlSync() {
    if (fastControlSyncTimer) return;
    fastControlSyncTimer = setTimeout(() => {
      fastControlSyncTimer = null;
      try {
        const sendBtn = findChatSendButton();
        if (!sendBtn) return;
        placeThinkingBySendButton(sendBtn);
        ensureAgenticButton();
      } catch (_e) {}
    }, 50);
  }

  function isInsideUserInput(node) {
    let el = node && (node.nodeType === 3 ? node.parentElement : node);
    while (el && el !== document.body) {
      const tag = el.tagName;
      if (tag === 'TEXTAREA' || tag === 'INPUT') return true;
      if (el.isContentEditable) return true;
      el = el.parentElement;
    }
    return false;
  }

  const observer = new MutationObserver((muts) => {
    let sawRelevantDomChange = false;
    let sawOurAgenticRemoved = false;
    for (const m of muts) {
      if (m.type === 'characterData' && m.target.nodeType === 3) {
        if (isInsideUserInput(m.target)) continue;
        translateNode(m.target);
        continue;
      }
      m.addedNodes.forEach((node) => {
        if (node.nodeType === 1) {
          if (isInsideUserInput(node)) return;
          sawRelevantDomChange = true;
          scanTextNodes(node);
          positionSendTooltip();
          if (node.querySelector?.(CHAT_SEND_SELECTOR) || node.querySelector?.('.cdxvi-thinking-toggle')) scheduleFastControlSync();
        } else if (node.nodeType === 3) {
          if (isInsideUserInput(node)) return;
          translateNode(node);
        }
      });
      m.removedNodes.forEach((node) => {
        if (node.nodeType !== 1) return;
        if (isInsideUserInput(node)) return;
        sawRelevantDomChange = true;
        if (node.classList && node.classList.contains('cdxvi-thinking-toggle')) scheduleFastControlSync();
        if (typeof node.querySelector === 'function' && node.querySelector('.cdxvi-thinking-toggle')) scheduleFastControlSync();
        if (node.classList && node.classList.contains('cdxvi-agentic-button')) {
          sawOurAgenticRemoved = true;
        } else if (typeof node.querySelector === 'function' && node.querySelector('.cdxvi-agentic-button')) {
          sawOurAgenticRemoved = true;
        }
      });
    }
    if (sawOurAgenticRemoved || sawRelevantDomChange) scheduleExpensiveSync();
  });

  function boot() {
    scanTextNodes(document.body);
    ensureAgenticButton();
    tagThinkingButtons();
    observer.observe(document.body, { childList: true, subtree: true, characterData: true });
    watchRoute();
    if (isNewChatPath(location.pathname)) {
      applyInitialThinkingDefault();
      applyStoredThinkingMode(4, 200);
    }

    setInterval(() => {
      ensureAgenticButton();
      tagThinkingButtons();
      if (isUserTyping()) return;
      applyStoredThinkingMode(2, 150);
    }, 700);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
