// web/js/app.js — AlgoTrade Local Web Dashboard Controller (ES6)
// Loopback communication strictly to 127.0.0.1:3000

class AlgoTradeApp {
  constructor() {
    this.ws = null;
    this.wsReconnectTimeout = null;
    this.wsReconnectAttempts = 0;
    this.currentStatus = null;
    this.currentAccount = null;
    this.currentCapital = null;
    this.proposalCountdownInterval = null;
    this.chart = null;

    this.initElements();
    this.initEventListeners();
    this.initTabs();
    this.initChart();
    this.fetchInitialData();
    this.connectWebSocket();
    this.startPeriodicRefresh();
  }

  initElements() {
    // Header Status
    this.accountAddressBadge = document.getElementById('account-address-badge');
    this.signerStatusBadge = document.getElementById('signer-status-badge');
    this.wsStatusPill = document.getElementById('ws-status-pill');
    this.wsStatusText = document.getElementById('ws-status-text');
    this.engineModeBadge = document.getElementById('engine-mode-badge');
    this.activeProfileBadge = document.getElementById('active-profile-badge');
    this.activeMarginBadge = document.getElementById('active-margin-badge');

    // Warning & Notification Banners
    this.signerWarningBanner = document.getElementById('signer-warning-banner');
    this.signerWarningTitle = document.getElementById('signer-warning-title');
    this.signerWarningText = document.getElementById('signer-warning-text');
    this.discrepancyBanner = document.getElementById('discrepancy-banner');
    this.discrepancyDetails = document.getElementById('discrepancy-details');
    this.proposalBanner = document.getElementById('proposal-banner');
    this.propDirection = document.getElementById('prop-direction');
    this.propPrice = document.getElementById('prop-price');
    this.propSize = document.getElementById('prop-size');
    this.propNotional = document.getElementById('prop-notional');
    this.propTp = document.getElementById('prop-tp');
    this.propSl = document.getElementById('prop-sl');
    this.propTimer = document.getElementById('prop-timer');
    this.btnApproveProposal = document.getElementById('btn-approve-proposal');
    this.btnRejectProposal = document.getElementById('btn-reject-proposal');

    // Panel 1: Account Overview
    this.accSignerBadge = document.getElementById('acc-signer-badge');
    this.accEquity = document.getElementById('acc-equity');
    this.accWithdrawable = document.getElementById('acc-withdrawable');
    this.accMarginUsed = document.getElementById('acc-margin-used');
    this.accUnrealizedPnl = document.getElementById('acc-unrealized-pnl');
    this.accOpenPosDesc = document.getElementById('acc-open-pos-desc');
    this.accFullAddress = document.getElementById('acc-full-address');
    this.perfFeedStatus = document.getElementById('perf-feed-status');

    // Panel 2: Bot Capital Ledger
    this.capAllocatedTrading = document.getElementById('cap-allocated-trading');
    this.capReserve = document.getElementById('cap-reserve');
    this.capCommittedMargin = document.getElementById('cap-committed-margin');
    this.capUncommitted = document.getElementById('cap-uncommitted');
    this.capRealizedNet = document.getElementById('cap-realized-net');
    this.capFeesPaid = document.getElementById('cap-fees-paid');
    this.capNetFunding = document.getElementById('cap-net-funding');
    this.inputReallocAmount = document.getElementById('input-realloc-amount');
    this.btnReallocToReserve = document.getElementById('btn-realloc-to-reserve');
    this.btnReallocToTrading = document.getElementById('btn-realloc-to-trading');
    this.reallocFeedback = document.getElementById('realloc-feedback');

    // Panel 3: Live BTC Market
    this.perfMarkPrice = document.getElementById('perf-mark-price');

    // Panel 4: Current Position and Protection
    this.posDirectionPill = document.getElementById('pos-direction-pill');
    this.perfPosState = document.getElementById('perf-pos-state');
    this.perfPosDetails = document.getElementById('perf-pos-details');
    this.perfUnrealizedPnl = document.getElementById('perf-unrealized-pnl');
    this.perfNetPnl = document.getElementById('perf-net-pnl');
    this.perfNetBreakdown = document.getElementById('perf-net-breakdown');
    this.perfLossStreak = document.getElementById('perf-loss-streak');
    this.perfTargetProgress = document.getElementById('perf-target-progress');
    this.perfActiveOrders = document.getElementById('perf-active-orders');
    this.perfActiveTp = document.getElementById('perf-active-tp');
    this.perfActiveSl = document.getElementById('perf-active-sl');
    this.perfProtectionStatus = document.getElementById('perf-protection-status');

    // Panel 5: Strategy & Risk Settings + Trade Preview + Controls
    this.configForm = document.getElementById('config-form');
    this.trailingStopToggle = document.getElementById('trailing-stop-toggle');
    this.seedInput = document.getElementById('seed-input');
    this.btnRandomSeed = document.getElementById('btn-random-seed');

    // Trade Preview Specs
    this.prevProfileDir = document.getElementById('prev-profile-dir');
    this.prevEntryPx = document.getElementById('prev-entry-px');
    this.prevSizeNotional = document.getElementById('prev-size-notional');
    this.prevMarginReq = document.getElementById('prev-margin-req');
    this.prevTpPx = document.getElementById('prev-tp-px');
    this.prevSlPx = document.getElementById('prev-sl-px');
    this.prevFeesEst = document.getElementById('prev-fees-est');
    this.prevNetTp = document.getElementById('prev-net-tp');
    this.prevNetSl = document.getElementById('prev-net-sl');
    this.btnRefreshPreview = document.getElementById('btn-refresh-preview');

    // Lifecycle Controls
    this.btnStart = document.getElementById('btn-control-start');
    this.btnPause = document.getElementById('btn-control-pause');
    this.btnStop = document.getElementById('btn-control-stop');
    this.btnFlatten = document.getElementById('btn-control-flatten');

    // Panel 6: Activity & Audit Stream
    this.tradesTbody = document.getElementById('trades-tbody');
    this.auditTerminal = document.getElementById('audit-log-terminal');

    // Modal
    this.flattenModal = document.getElementById('flatten-modal');
    this.btnCloseModal = document.getElementById('btn-close-modal');
    this.btnCancelFlatten = document.getElementById('btn-cancel-flatten');
    this.btnConfirmFlatten = document.getElementById('btn-confirm-flatten');
  }

  initChart() {
    if (window.BtcMarketChart) {
      this.chart = new window.BtcMarketChart('tv-chart-container');
    }
  }

  initTabs() {
    const tabButtons = document.querySelectorAll('.tab-btn');
    const viewPanels = document.querySelectorAll('.view-panel');

    tabButtons.forEach((btn) => {
      btn.addEventListener('click', () => {
        const targetViewId = btn.getAttribute('data-tab');
        tabButtons.forEach((b) => b.classList.remove('active'));
        viewPanels.forEach((p) => p.classList.remove('active'));

        btn.classList.add('active');
        const targetPanel = document.getElementById(targetViewId);
        if (targetPanel) {
          targetPanel.classList.add('active');
        }
      });
    });
  }

  initEventListeners() {
    // Config profile change
    document.querySelectorAll('input[name="profile"]').forEach((radio) => {
      radio.addEventListener('change', (e) => {
        if (e.target.value === 'GREEDY') {
          this.trailingStopToggle.checked = true;
        } else {
          this.trailingStopToggle.checked = false;
        }
        this.fetchTradePreview();
      });
    });

    document.querySelectorAll('input[name="margin_mode"]').forEach((radio) => {
      radio.addEventListener('change', () => this.fetchTradePreview());
    });

    // Random seed generator
    this.btnRandomSeed.addEventListener('click', () => {
      this.seedInput.value = Math.floor(Math.random() * 900000) + 100000;
      this.fetchTradePreview();
    });

    // Save/Stage Config
    this.configForm.addEventListener('submit', (e) => this.handleSaveConfig(e));

    // Refresh trade preview
    this.btnRefreshPreview.addEventListener('click', () => this.fetchTradePreview());

    // Capital Reallocation
    this.btnReallocToReserve.addEventListener('click', () => this.handleReallocateCapital('ToReserve'));
    this.btnReallocToTrading.addEventListener('click', () => this.handleReallocateCapital('ToTrading'));

    // Lifecycle Controls
    this.btnStart.addEventListener('click', () => this.handleStartEngine());
    this.btnPause.addEventListener('click', () => this.handlePauseEngine());
    this.btnStop.addEventListener('click', () => this.handleStopEngine());

    // Emergency Flatten Modal Controls
    this.btnFlatten.addEventListener('click', () => this.flattenModal.classList.remove('hidden'));
    this.btnCloseModal.addEventListener('click', () => this.flattenModal.classList.add('hidden'));
    this.btnCancelFlatten.addEventListener('click', () => this.flattenModal.classList.add('hidden'));
    this.btnConfirmFlatten.addEventListener('click', () => this.handleConfirmEmergencyFlatten());

    // Pending Proposal Actions
    this.btnApproveProposal.addEventListener('click', () => this.handleApproveProposal());
    this.btnRejectProposal.addEventListener('click', () => this.handleRejectProposal());
  }

  async fetchInitialData() {
    try {
      const [statusRes, accRes, capRes, prevRes, histRes, logsRes] = await Promise.all([
        fetch('/api/status').then((r) => r.json()).catch(() => null),
        fetch('/api/account').then((r) => r.json()).catch(() => null),
        fetch('/api/capital').then((r) => r.json()).catch(() => null),
        fetch('/api/preview').then((r) => r.json()).catch(() => null),
        fetch('/api/history').then((r) => r.json()).catch(() => null),
        fetch('/api/logs').then((r) => r.json()).catch(() => null),
      ]);

      if (statusRes && statusRes.success && statusRes.data) {
        this.renderStatus(statusRes.data);
      }
      if (accRes) {
        if (accRes.success && accRes.data) {
          this.renderAccount(accRes.data);
        } else {
          this.renderAccountUnavailable(accRes.error || 'Failed to query account state', 'UNAVAILABLE');
        }
      }
      if (capRes && capRes.success && capRes.data) {
        this.renderCapital(capRes.data);
      }
      if (prevRes) {
        if (prevRes.success && prevRes.data) {
          this.renderTradePreview(prevRes.data);
        } else {
          this.renderPreviewUnavailable(prevRes.error || 'Preview unavailable');
        }
      }
      if (histRes && histRes.success && histRes.data) {
        this.renderTradeHistory(histRes.data);
      }
      if (logsRes && logsRes.success && Array.isArray(logsRes.data)) {
        logsRes.data.forEach((log) => this.appendLogEvent(log));
      }
    } catch (err) {
      console.warn('Initial data fetch notice:', err);
    }
  }

  startPeriodicRefresh() {
    // Poll auxiliary endpoints every 5 seconds
    setInterval(async () => {
      try {
        const [accRes, capRes, prevRes] = await Promise.all([
          fetch('/api/account').then((r) => r.json()).catch(() => null),
          fetch('/api/capital').then((r) => r.json()).catch(() => null),
          fetch('/api/preview').then((r) => r.json()).catch(() => null),
        ]);

        if (accRes) {
          if (accRes.success && accRes.data) {
            this.renderAccount(accRes.data);
          } else {
            this.renderAccountUnavailable(accRes.error || 'Failed to query account state', 'UNAVAILABLE');
          }
        }
        if (capRes && capRes.success && capRes.data) this.renderCapital(capRes.data);
        if (prevRes) {
          if (prevRes.success && prevRes.data) {
            this.renderTradePreview(prevRes.data);
          } else {
            this.renderPreviewUnavailable(prevRes.error || 'Preview unavailable');
          }
        }
      } catch (e) {
        // quiet background poll
      }
    }, 5000);
  }

  connectWebSocket() {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsUrl = `${protocol}//${window.location.host}/api/ws`;

    this.ws = new WebSocket(wsUrl);

    this.ws.onopen = () => {
      this.wsReconnectAttempts = 0;
      this.wsStatusPill.querySelector('.status-dot').className = 'status-dot connected';
      this.wsStatusText.textContent = 'Connected (127.0.0.1)';
    };

    this.ws.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data);
        if (payload.mode !== undefined && payload.position !== undefined) {
          // Status snapshot update
          this.renderStatus(payload);
        } else if (payload.event_type !== undefined || payload.event !== undefined) {
          // Real-time audit log event
          this.appendLogEvent(payload);
        } else if (payload.type === 'candle_update' && payload.candle) {
          // Real-time candlestick update
          if (this.chart) {
            this.chart.onCandleSnapshot(payload.candle);
          }
        }
      } catch (err) {
        console.error('Failed to parse WebSocket message:', err);
      }
    };

    this.ws.onclose = () => {
      this.wsStatusPill.querySelector('.status-dot').className = 'status-dot disconnected';
      this.wsStatusText.textContent = 'Reconnecting...';
      this.scheduleReconnect();
    };

    this.ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      this.ws.close();
    };
  }

  scheduleReconnect() {
    if (this.wsReconnectTimeout) return;
    const delay = Math.min(1000 * Math.pow(1.5, this.wsReconnectAttempts), 10000);
    this.wsReconnectAttempts++;
    this.wsReconnectTimeout = setTimeout(() => {
      this.wsReconnectTimeout = null;
      this.connectWebSocket();
    }, delay);
  }

  // ============================================================================
  // Rendering Methods
  // ============================================================================

  renderStatus(status) {
    this.currentStatus = status;

    // 1. Engine Mode Badge
    const mode = typeof status.mode === 'string' ? status.mode : Object.keys(status.mode)[0];
    const modeUpper = mode.toUpperCase();
    this.engineModeBadge.textContent = modeUpper;
    this.engineModeBadge.className = `badge-value ${mode.toLowerCase()}`;

    // 2. Active Profile and Margin Mode Badges
    const profileName = status.config.profile.profile;
    const marginMode = status.config.margin_mode;
    this.activeProfileBadge.textContent = profileName;
    this.activeMarginBadge.textContent = marginMode.toUpperCase();

    // 3. Mark Price & Feed Freshness
    if (status.mark_price) {
      const priceNum = parseFloat(status.mark_price);
      this.perfMarkPrice.textContent = `$${priceNum.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}`;
      if (this.chart) {
        this.chart.updateLiveMarkPrice(priceNum);
      }
    } else {
      this.perfMarkPrice.textContent = 'Awaiting...';
    }

    if (status.is_stale) {
      this.perfFeedStatus.innerHTML = `<span class="dot-indicator red"></span> <span class="text-danger">Stale (${status.mark_price_age_ms || 0}ms)</span>`;
    } else {
      this.perfFeedStatus.innerHTML = `<span class="dot-indicator green"></span> <span class="text-success">Fresh (${status.mark_price_age_ms || 0}ms)</span>`;
    }

    // 4. Position State
    let posStr = 'FLAT';
    let posSize = '0.0000 BTC';
    let posEntry = '$0.00';

    if (typeof status.position === 'object') {
      if ('OpenProtected' in status.position) {
        const op = status.position.OpenProtected;
        posStr = `${op.direction.toUpperCase()}`;
        posSize = `${op.size} BTC`;
        posEntry = `$${op.avg_entry_px}`;
      } else if ('Opening' in status.position) {
        posStr = `OPENING (${status.position.Opening.direction})`;
      } else if ('Closing' in status.position) {
        posStr = 'CLOSING';
      }
    }

    this.posDirectionPill.textContent = posStr;
    this.posDirectionPill.className = `badge-value ${posStr === 'LONG' ? 'running' : posStr === 'SHORT' ? 'stopped' : ''}`;
    this.perfPosState.textContent = posStr;
    this.perfPosState.className = `stat-value font-mono ${posStr === 'LONG' ? 'text-success' : posStr === 'SHORT' ? 'text-danger' : ''}`;
    this.perfPosDetails.textContent = `Size: ${posSize} | Entry: ${posEntry}`;

    // 5. Unrealized and Realized P&L
    const unrealized = parseFloat(status.unrealized_pnl || '0');
    this.perfUnrealizedPnl.textContent = `${unrealized >= 0 ? '+' : ''}$${unrealized.toFixed(4)}`;
    this.perfUnrealizedPnl.className = `stat-value font-mono ${unrealized >= 0 ? 'text-success' : 'text-danger'}`;

    const realized = parseFloat(status.total_realized_net_pnl || '0');
    this.perfNetPnl.textContent = `${realized >= 0 ? '+' : ''}$${realized.toFixed(4)}`;
    this.perfNetPnl.className = `stat-value font-mono ${realized >= 0 ? 'text-success' : 'text-danger'}`;

    const fees = parseFloat(status.total_fees_paid || '0');
    const funding = parseFloat(status.total_signed_funding || '0');
    this.perfNetBreakdown.textContent = `Fees: -$${fees.toFixed(4)} | Funding: ${funding >= 0 ? '+' : ''}$${funding.toFixed(4)}`;

    // 6. Loss Streak and Profit Target Progress
    this.perfLossStreak.textContent = `${status.consecutive_losses} / ${status.config.profile.consecutive_losses_limit}`;
    this.perfTargetProgress.textContent = `Target: $${realized.toFixed(2)} / $${status.config.profile.net_session_profit_target}`;

    // 7. Active Protection Orders
    if (status.active_tp_oid || status.active_sl_oid) {
      this.perfActiveOrders.textContent = 'Active & Protected';
      this.perfActiveOrders.className = 'badge-tag success';
      this.perfActiveTp.textContent = `#${status.active_tp_oid || 'RESTING'}`;
      this.perfActiveSl.textContent = `#${status.active_sl_oid || 'RESTING'}`;
      this.perfProtectionStatus.textContent = 'Position backed by active reduce-only exchange triggers.';
    } else {
      this.perfActiveOrders.textContent = 'None';
      this.perfActiveOrders.className = 'badge-tag';
      this.perfActiveTp.textContent = 'None';
      this.perfActiveSl.textContent = 'None';
      this.perfProtectionStatus.textContent = 'When open, positions are strictly backed by resting reduce-only stop triggers.';
    }

    // 8. Reconciliation Discrepancy Banner
    if (status.last_discrepancies && status.last_discrepancies.length > 0) {
      this.discrepancyBanner.classList.remove('hidden');
      this.discrepancyDetails.textContent = status.last_discrepancies.join('; ');
    } else {
      this.discrepancyBanner.classList.add('hidden');
    }

    // 9. Pending Confirmation Proposal
    if (status.pending_proposal) {
      this.renderPendingProposal(status.pending_proposal);
    } else {
      this.proposalBanner.classList.add('hidden');
      if (this.proposalCountdownInterval) {
        clearInterval(this.proposalCountdownInterval);
        this.proposalCountdownInterval = null;
      }
    }

    // 10. Update Control Button States
    const isRunning = mode.toLowerCase() === 'running';
    this.btnStart.disabled = isRunning || (this.currentAccount && this.currentAccount.signer_status !== 'Active');
    this.btnPause.disabled = !isRunning;
  }

  renderAccount(acc) {
    if (!acc) return;
    this.currentAccount = acc;

    // Distinguish environment states: LIVE, MOCK/DEMO, UNCONFIGURED, STALE
    const isMock = !!acc.is_mock;
    const isUnconfigured = !acc.account_address ||
      acc.account_address === '0x0000000000000000000000000000000000000000' ||
      acc.account_address === '0x0000000000000000000000000000000000000001';
    const isStale = !!acc.exchange_time_ms && (Date.now() - acc.exchange_time_ms > 30000);

    let envBadgeText = 'LIVE TESTNET';
    if (isMock) {
      envBadgeText = 'MOCK / DEMO';
    } else if (isUnconfigured) {
      envBadgeText = 'UNCONFIGURED';
    } else if (isStale) {
      envBadgeText = 'STALE LIVE';
    }

    // Shortened address badge in header
    this.accountAddressBadge.textContent = isMock ? 'MOCK' : (acc.shortened_address || '--');
    this.accountAddressBadge.title = `${envBadgeText} — ${acc.source || 'Engine'}`;
    this.accFullAddress.textContent = `${acc.account_address || '--'} (${envBadgeText})`;

    // Signer Status Badge & Warnings
    const signerSt = acc.signer_status || 'Unconfigured';
    this.signerStatusBadge.textContent = signerSt.toUpperCase();

    if (signerSt === 'Active') {
      this.signerStatusBadge.className = 'badge-value running';
      this.accSignerBadge.textContent = isMock ? 'Mock Signer Active' : 'Signer Active (30-day)';
      this.accSignerBadge.className = isMock ? 'badge-tag warning' : 'badge-tag success';
      this.signerWarningBanner.classList.add('hidden');
      this.btnStart.title = 'Start trading engine';
      if (this.currentStatus && this.currentStatus.mode.toLowerCase() !== 'running') {
        this.btnStart.disabled = false;
      }
    } else {
      this.signerStatusBadge.className = 'badge-value stopped';
      this.accSignerBadge.textContent = `Signer ${signerSt}`;
      this.accSignerBadge.className = 'badge-tag danger';
      this.signerWarningBanner.classList.remove('hidden');
      this.signerWarningTitle.textContent = `Signer Status: ${signerSt} — Order Submissions Disabled`;
      this.signerWarningText.textContent =
        `The agent wallet signer is ${signerSt}. Trading actions are gated. ` +
        `Hyperliquid agent wallet authorizations have an intended 30-day lifecycle and must be renewed on the exchange when expired.`;
      this.btnStart.disabled = true;
      this.btnStart.title = `Engine start disabled: Agent signer is ${signerSt}`;
      this.btnApproveProposal.disabled = true;
    }

    // Account metrics with proper domain field names
    const equity = parseFloat(acc.account_value !== undefined ? acc.account_value : (acc.equity || '0'));
    const withdrawable = parseFloat(acc.withdrawable !== undefined ? acc.withdrawable : (acc.available_balance || '0'));
    const marginUsed = parseFloat(acc.total_margin_used !== undefined ? acc.total_margin_used : (acc.margin_used || '0'));

    this.accEquity.textContent = `$${equity.toFixed(2)} USDC`;
    this.accWithdrawable.textContent = `$${withdrawable.toFixed(2)} USDC`;
    this.accMarginUsed.textContent = `$${marginUsed.toFixed(2)} USDC`;

    let unrealized = 0;
    let posDesc = 'Exchange position flat';

    if (acc.positions && Array.isArray(acc.positions) && acc.positions.length > 0) {
      const pos = acc.positions[0];
      const szi = parseFloat(pos.szi || '0');
      if (szi !== 0) {
        unrealized = parseFloat(pos.unrealized_pnl || '0');
        const dir = pos.is_long ? 'LONG' : 'SHORT';
        const entryPx = pos.entry_px ? parseFloat(pos.entry_px).toFixed(1) : '--';
        posDesc = `${dir} ${Math.abs(szi)} BTC @ $${entryPx}`;
      }
    } else if (acc.open_position_size && parseFloat(acc.open_position_size) !== 0) {
      posDesc = `${acc.open_position_size} BTC @ $${acc.open_position_entry}`;
      unrealized = parseFloat(acc.unrealized_pnl || '0');
    }

    this.accUnrealizedPnl.textContent = `${unrealized >= 0 ? '+' : ''}$${unrealized.toFixed(2)} USDC`;
    this.accUnrealizedPnl.className = `stat-value font-mono ${unrealized >= 0 ? 'text-success' : 'text-danger'}`;
    this.accOpenPosDesc.textContent = posDesc;
  }

  renderAccountUnavailable(errMsg, stateLabel = 'UNAVAILABLE') {
    this.accountAddressBadge.textContent = stateLabel;
    this.accountAddressBadge.className = 'badge-value font-mono text-danger';
    this.signerStatusBadge.textContent = stateLabel;
    this.signerStatusBadge.className = 'badge-value stopped';
    this.accSignerBadge.textContent = stateLabel;
    this.accSignerBadge.className = 'badge-tag danger';
    this.accEquity.textContent = '--';
    this.accWithdrawable.textContent = '--';
    this.accMarginUsed.textContent = '--';
    this.accUnrealizedPnl.textContent = '--';
    this.accOpenPosDesc.textContent = errMsg || 'Exchange state unavailable';
    this.signerWarningBanner.classList.remove('hidden');
    this.signerWarningTitle.textContent = `Exchange State ${stateLabel}`;
    this.signerWarningText.textContent = errMsg || 'Could not query Hyperliquid testnet endpoint.';
    this.btnStart.disabled = true;
    this.btnApproveProposal.disabled = true;
  }

  renderCapital(cap) {
    if (!cap) return;
    this.currentCapital = cap;

    const allocatedTrading = parseFloat(cap.allocated_trading || '0');
    const reserve = parseFloat(cap.allocated_reserve !== undefined ? cap.allocated_reserve : (cap.reserve || '0'));
    const committed = parseFloat(cap.committed_margin || '0');
    const uncommitted = parseFloat(cap.uncommitted_trading || '0');
    const realizedNet = parseFloat(cap.realized_net_pnl || '0');
    const fees = parseFloat(cap.total_fees_paid || '0');
    const funding = parseFloat(cap.net_funding || '0');

    this.capAllocatedTrading.textContent = `$${allocatedTrading.toFixed(2)} USDC`;
    this.capReserve.textContent = `$${reserve.toFixed(2)} USDC`;
    this.capCommittedMargin.textContent = `$${committed.toFixed(2)} USDC`;
    this.capUncommitted.textContent = `$${uncommitted.toFixed(2)} USDC`;

    this.capRealizedNet.textContent = `${realizedNet >= 0 ? '+' : ''}$${realizedNet.toFixed(4)}`;
    this.capRealizedNet.className = `strip-value font-mono ${realizedNet >= 0 ? 'text-success' : 'text-danger'}`;
    this.capFeesPaid.textContent = `-$${fees.toFixed(4)}`;
    this.capNetFunding.textContent = `${funding >= 0 ? '+' : ''}$${funding.toFixed(4)}`;
    this.capNetFunding.className = `strip-value font-mono ${funding >= 0 ? 'text-success' : 'text-danger'}`;
  }

  renderTradePreview(prev) {
    if (!prev || typeof prev !== 'object') {
      this.renderPreviewUnavailable('No preview data received');
      return;
    }

    const missingFields = [];
    if (!prev.direction) missingFields.push('direction');
    if (prev.entry_price === undefined) missingFields.push('entry_price');
    if (prev.quantity === undefined && prev.size === undefined) missingFields.push('quantity');
    if (prev.estimated_margin === undefined && prev.margin_required === undefined) missingFields.push('estimated_margin');
    if (prev.tp_price === undefined) missingFields.push('tp_price');
    if (prev.sl_price === undefined) missingFields.push('sl_price');

    if (missingFields.length > 0) {
      this.renderPreviewUnavailable(`Missing fields: ${missingFields.join(', ')}`);
      return;
    }

    const profile = prev.profile || 'NORMAL';
    const direction = (prev.direction || 'LONG').toUpperCase();
    const marginMode = prev.margin_mode || 'ISOLATED';
    const leverage = prev.leverage || 5;

    const entryPx = parseFloat(prev.entry_price);
    const qty = parseFloat(prev.quantity !== undefined ? prev.quantity : prev.size);
    const notional = parseFloat(prev.notional !== undefined ? prev.notional : (entryPx * qty));
    const marginReq = parseFloat(prev.estimated_margin !== undefined ? prev.estimated_margin : prev.margin_required);
    const tpPx = parseFloat(prev.tp_price);
    const slPx = parseFloat(prev.sl_price);

    // Entry fee + exit fee estimates
    const entryFee = parseFloat(prev.estimated_entry_fee || '0');
    const exitFeeTp = parseFloat(prev.estimated_exit_fee_tp || '0');
    const totalTakerFee = entryFee + exitFeeTp;
    const feeDisplay = prev.est_taker_fee_total !== undefined ? parseFloat(prev.est_taker_fee_total) : totalTakerFee;

    const netTp = parseFloat(prev.estimated_tp_net_pnl !== undefined ? prev.estimated_tp_net_pnl : (prev.est_net_pnl_tp || '0'));
    const netSl = parseFloat(prev.estimated_sl_net_pnl !== undefined ? prev.estimated_sl_net_pnl : (prev.est_net_pnl_sl || '0'));

    if (isNaN(entryPx) || isNaN(qty) || isNaN(marginReq) || isNaN(tpPx) || isNaN(slPx)) {
      this.renderPreviewUnavailable('Invalid calculations in trade preview');
      return;
    }

    this.prevProfileDir.textContent = `${profile} • ${direction}`;
    this.prevEntryPx.textContent = `$${entryPx.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}`;
    this.prevSizeNotional.textContent = `${qty} BTC ($${notional.toFixed(2)})`;
    this.prevMarginReq.textContent = `$${marginReq.toFixed(2)} USDC (${leverage}x ${marginMode})`;

    this.prevTpPx.textContent = `$${tpPx.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}`;
    this.prevSlPx.textContent = `$${slPx.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}`;
    this.prevFeesEst.textContent = `-$${feeDisplay.toFixed(4)}`;

    this.prevNetTp.textContent = `${netTp >= 0 ? '+' : ''}$${netTp.toFixed(4)}`;
    this.prevNetTp.className = `strip-value font-mono ${netTp >= 0 ? 'text-success' : 'text-danger'}`;
    this.prevNetSl.textContent = `${netSl >= 0 ? '+' : ''}$${netSl.toFixed(4)}`;
    this.prevNetSl.className = `strip-value font-mono ${netSl >= 0 ? 'text-success' : 'text-danger'}`;
  }

  renderPreviewUnavailable(reason) {
    this.prevProfileDir.textContent = 'Preview Unavailable';
    this.prevEntryPx.textContent = '--';
    this.prevSizeNotional.textContent = reason ? `Unavailable: ${reason}` : 'No mark price or sizing error';
    this.prevMarginReq.textContent = '--';
    this.prevTpPx.textContent = '--';
    this.prevSlPx.textContent = '--';
    this.prevFeesEst.textContent = '--';
    this.prevNetTp.textContent = '--';
    this.prevNetSl.textContent = '--';
  }

  renderPendingProposal(proposal) {
    this.proposalBanner.classList.remove('hidden');
    this.propDirection.textContent = proposal.direction;
    this.propPrice.textContent = `$${proposal.proposed_entry_price}`;
    this.propSize.textContent = `${proposal.size} BTC`;
    this.propNotional.textContent = `$${proposal.notional}`;
    this.propTp.textContent = `$${proposal.tp_price}`;
    this.propSl.textContent = `$${proposal.sl_price}`;

    // Update Countdown
    const updateTimer = () => {
      const expiry = new Date(proposal.expires_at).getTime();
      const now = Date.now();
      const remainingSec = Math.max(0, Math.round((expiry - now) / 1000));
      this.propTimer.textContent = `${remainingSec}s`;

      if (remainingSec <= 0) {
        this.propTimer.textContent = 'EXPIRED';
        this.btnApproveProposal.disabled = true;
      } else {
        const canApprove = this.currentAccount && this.currentAccount.signer_status === 'Active';
        this.btnApproveProposal.disabled = !canApprove;
      }
    };

    updateTimer();
    if (this.proposalCountdownInterval) clearInterval(this.proposalCountdownInterval);
    this.proposalCountdownInterval = setInterval(updateTimer, 1000);
  }

  renderTradeHistory(trades) {
    if (!trades || trades.length === 0) {
      this.tradesTbody.innerHTML = `<tr><td colspan="7" class="text-center empty-state">No closed trades recorded in current session</td></tr>`;
      return;
    }

    this.tradesTbody.innerHTML = trades
      .map((trade) => {
        const gross = parseFloat(trade.gross_pnl);
        const net = parseFloat(trade.net_pnl);
        const fee = parseFloat(trade.entry_fee) + parseFloat(trade.exit_fee);

        return `
        <tr>
          <td class="${trade.direction === 'Long' ? 'text-success' : 'text-danger'} font-semibold">${trade.direction}</td>
          <td>$${trade.entry_price}</td>
          <td>$${trade.exit_price}</td>
          <td>${trade.quantity}</td>
          <td class="${gross >= 0 ? 'text-success' : 'text-danger'}">${gross >= 0 ? '+' : ''}$${gross.toFixed(4)}</td>
          <td>$${fee.toFixed(4)}</td>
          <td class="${net >= 0 ? 'text-success' : 'text-danger'} font-semibold">${net >= 0 ? '+' : ''}$${net.toFixed(4)}</td>
        </tr>
      `;
      })
      .join('');
  }

  appendLogEvent(event) {
    const entry = document.createElement('div');
    const ts = new Date().toLocaleTimeString();
    const eventType = event.event ? Object.keys(event.event)[0] : 'Telemetry';
    const eventData = event.event ? event.event[eventType] : event;

    let typeClass = 'system';
    if (eventType.includes('Order')) typeClass = 'order';
    if (eventType.includes('Fill')) typeClass = 'fill';
    if (eventType.includes('Risk') || eventType.includes('Halt')) typeClass = 'risk';
    if (eventType.includes('Protection')) typeClass = 'protection';

    entry.className = `log-entry ${typeClass}`;
    entry.textContent = `[${ts}] [${eventType.toUpperCase()}] ${JSON.stringify(eventData)}`;

    this.auditTerminal.appendChild(entry);
    this.auditTerminal.scrollTop = this.auditTerminal.scrollHeight;
  }

  // ============================================================================
  // Action Handlers
  // ============================================================================

  async fetchTradePreview() {
    try {
      const res = await fetch('/api/preview').then((r) => r.json());
      if (res.success && res.data) {
        this.renderTradePreview(res.data);
      }
    } catch (err) {
      console.warn('Failed to refresh trade preview:', err);
    }
  }

  async handleReallocateCapital(direction) {
    const amountVal = parseFloat(this.inputReallocAmount.value);
    if (isNaN(amountVal) || amountVal <= 0) {
      this.showReallocFeedback('Please enter a valid amount greater than $0', false);
      return;
    }

    try {
      const res = await fetch('/api/capital/reallocate', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          direction: direction,
          amount: amountVal.toFixed(2),
        }),
      }).then((r) => r.json());

      if (res.success && res.data) {
        this.renderCapital(res.data);
        this.showReallocFeedback(`Transferred $${amountVal.toFixed(2)} ${direction === 'ToReserve' ? 'to reserve' : 'to trading'}.`, true);
        this.fetchTradePreview();
      } else {
        this.showReallocFeedback(`Reallocation error: ${res.error}`, false);
      }
    } catch (err) {
      this.showReallocFeedback(`Network error: ${err}`, false);
    }
  }

  showReallocFeedback(msg, isSuccess) {
    this.reallocFeedback.textContent = msg;
    this.reallocFeedback.className = `feedback-msg ${isSuccess ? 'success' : 'error'}`;
    this.reallocFeedback.classList.remove('hidden');
    setTimeout(() => {
      this.reallocFeedback.classList.add('hidden');
    }, 4500);
  }

  async handleSaveConfig(e) {
    e.preventDefault();
    const formData = new FormData(this.configForm);
    const profile = formData.get('profile');
    const margin_mode = formData.get('margin_mode');
    const execution_mode = formData.get('execution_mode');
    const trailing_stop = this.trailingStopToggle ? this.trailingStopToggle.checked : false;
    const initial_seed = parseInt(this.seedInput.value, 10) || 42;

    const payload = {
      profile,
      margin_mode,
      execution_mode,
      trailing_stop,
      initial_seed,
    };

    try {
      const res = await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      }).then((r) => r.json());

      if (res.success) {
        alert('Configuration validated and staged successfully.');
        this.fetchTradePreview();
      } else {
        alert(`Config Error: ${res.error}`);
      }
    } catch (err) {
      alert(`Network error saving config: ${err}`);
    }
  }

  async handleStartEngine() {
    try {
      const res = await fetch('/api/control/start', { method: 'POST' }).then((r) => r.json());
      if (res.success) {
        console.log('Engine started successfully');
      } else {
        alert(`Failed to start engine: ${res.error}`);
      }
    } catch (err) {
      alert(`Network error starting engine: ${err}`);
    }
  }

  async handlePauseEngine() {
    try {
      const res = await fetch('/api/control/pause', { method: 'POST' }).then((r) => r.json());
      if (!res.success) alert(`Failed to pause: ${res.error}`);
    } catch (err) {
      alert(`Network error: ${err}`);
    }
  }

  async handleStopEngine() {
    try {
      const res = await fetch('/api/control/stop', { method: 'POST' }).then((r) => r.json());
      if (!res.success) alert(`Failed to stop: ${res.error}`);
    } catch (err) {
      alert(`Network error: ${err}`);
    }
  }

  async handleConfirmEmergencyFlatten() {
    this.flattenModal.classList.add('hidden');
    try {
      const res = await fetch('/api/control/emergency-flatten', { method: 'POST' }).then((r) => r.json());
      if (res.success) {
        alert('Emergency Flatten dispatched. Position market-closed and engine halted.');
      } else {
        alert(`Emergency Flatten error: ${res.error}`);
      }
    } catch (err) {
      alert(`Network error during flatten: ${err}`);
    }
  }

  async handleApproveProposal() {
    if (!this.currentStatus || !this.currentStatus.pending_proposal) return;
    const proposal_id = this.currentStatus.pending_proposal.proposal_id;

    try {
      const res = await fetch('/api/control/approve-entry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ proposal_id }),
      }).then((r) => r.json());

      if (!res.success) {
        alert(`Proposal Approval Rejected: ${res.error}`);
      }
    } catch (err) {
      alert(`Network error approving entry: ${err}`);
    }
  }

  async handleRejectProposal() {
    if (!this.currentStatus || !this.currentStatus.pending_proposal) return;
    const proposal_id = this.currentStatus.pending_proposal.proposal_id;

    try {
      const res = await fetch('/api/control/reject-entry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ proposal_id, reason: 'Manual rejection by operator' }),
      }).then((r) => r.json());

      if (!res.success) {
        alert(`Proposal Rejection Error: ${res.error}`);
      }
    } catch (err) {
      alert(`Network error rejecting proposal: ${err}`);
    }
  }
}

// Initialize Application once DOM loads
window.addEventListener('DOMContentLoaded', () => {
  window.app = new AlgoTradeApp();
});
