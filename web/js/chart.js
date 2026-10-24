// web/js/chart.js — TradingView Lightweight Charts Controller for BTC Perpetual Market
// Powered by TradingView Lightweight Charts™ under Apache License 2.0

class BtcMarketChart {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.chart = null;
    this.candlestickSeries = null;
    this.volumeSeries = null;
    this.activeInterval = '1m';
    this.lastBars = [];
    this.currentBar = null;
    this.pollTimer = null;

    if (this.container && window.LightweightCharts) {
      this.initChart();
      this.initTimeframeButtons();
      this.loadTimeframe(this.activeInterval);
      this.startPolling();
    } else {
      console.warn('LightweightCharts library or container not ready yet.');
    }
  }

  initChart() {
    const width = this.container.clientWidth || 600;
    const height = this.container.clientHeight || 340;

    this.chart = LightweightCharts.createChart(this.container, {
      width: width,
      height: height,
      layout: {
        background: { type: 'solid', color: '#090d16' },
        textColor: '#94a3b8',
        fontSize: 12,
        fontFamily: "'JetBrains Mono', 'Plus Jakarta Sans', monospace",
      },
      grid: {
        vertLines: { color: 'rgba(255, 255, 255, 0.04)' },
        horzLines: { color: 'rgba(255, 255, 255, 0.04)' },
      },
      crosshair: {
        mode: LightweightCharts.CrosshairMode.Normal,
        vertLine: {
          color: 'rgba(99, 102, 241, 0.5)',
          width: 1,
          style: LightweightCharts.LineStyle.Dashed,
        },
        horzLine: {
          color: 'rgba(99, 102, 241, 0.5)',
          width: 1,
          style: LightweightCharts.LineStyle.Dashed,
        },
      },
      rightPriceScale: {
        borderColor: 'rgba(255, 255, 255, 0.08)',
        scaleMargins: {
          top: 0.1,
          bottom: 0.25,
        },
      },
      timeScale: {
        borderColor: 'rgba(255, 255, 255, 0.08)',
        timeVisible: true,
        secondsVisible: false,
      },
      handleScroll: true,
      handleScale: true,
    });

    // Candlestick Series
    this.candlestickSeries = this.chart.addCandlestickSeries({
      upColor: '#10b981',
      downColor: '#ef4444',
      borderVisible: false,
      wickUpColor: '#10b981',
      wickDownColor: '#ef4444',
      priceFormat: {
        type: 'price',
        precision: 1,
        minMove: 0.1,
      },
    });

    // Volume Histogram (Overlayed on bottom)
    this.volumeSeries = this.chart.addHistogramSeries({
      priceFormat: {
        type: 'volume',
      },
      priceScaleId: '', // Overlay mode
      scaleMargins: {
        top: 0.8,
        bottom: 0,
      },
    });

    // Handle responsive container resize
    if (window.ResizeObserver) {
      const resizeObserver = new ResizeObserver((entries) => {
        if (!entries || entries.length === 0) return;
        const entry = entries[0];
        const newWidth = entry.contentRect.width;
        const newHeight = entry.contentRect.height;
        if (newWidth > 0 && newHeight > 0) {
          this.chart.applyOptions({ width: newWidth, height: newHeight });
        }
      });
      resizeObserver.observe(this.container);
    } else {
      window.addEventListener('resize', () => {
        if (this.container) {
          this.chart.applyOptions({
            width: this.container.clientWidth,
            height: this.container.clientHeight,
          });
        }
      });
    }
  }

  initTimeframeButtons() {
    const buttons = document.querySelectorAll('.tf-btn');
    buttons.forEach((btn) => {
      btn.addEventListener('click', () => {
        const tf = btn.getAttribute('data-interval');
        if (tf && tf !== this.activeInterval) {
          buttons.forEach((b) => b.classList.remove('active'));
          btn.classList.add('active');
          this.activeInterval = tf;
          this.loadTimeframe(tf);
        }
      });
    });
  }

  async loadTimeframe(interval) {
    try {
      const res = await fetch(`/api/candles?interval=${encodeURIComponent(interval)}&limit=300`).then((r) => r.json());
      if (res.success && Array.isArray(res.data)) {
        this.renderCandles(res.data);
      }
    } catch (err) {
      console.warn(`Failed to fetch candles for ${interval}:`, err);
    }
  }

  renderCandles(candles) {
    if (!candles || candles.length === 0) return;

    // Transform and sanitize candles
    const formattedCandles = [];
    const formattedVolumes = [];

    candles.forEach((c) => {
      const rawTs = parseInt(c.open_time_ms || c.timestamp || c.time, 10);
      const timeInSec = Math.floor(rawTs > 1e11 ? rawTs / 1000 : rawTs);
      const open = parseFloat(c.open);
      const high = parseFloat(c.high);
      const low = parseFloat(c.low);
      const close = parseFloat(c.close);
      const vol = parseFloat(c.volume || '0');

      if (!isNaN(timeInSec) && !isNaN(open) && !isNaN(high) && !isNaN(low) && !isNaN(close)) {
        formattedCandles.push({
          time: timeInSec,
          open: open,
          high: high,
          low: low,
          close: close,
        });

        formattedVolumes.push({
          time: timeInSec,
          value: vol,
          color: close >= open ? 'rgba(16, 185, 129, 0.35)' : 'rgba(239, 68, 68, 0.35)',
        });
      }
    });

    // Sort ascending by timestamp
    formattedCandles.sort((a, b) => a.time - b.time);
    formattedVolumes.sort((a, b) => a.time - b.time);

    // Deduplicate any overlapping timestamps
    const dedupedCandles = [];
    const dedupedVolumes = [];
    for (let i = 0; i < formattedCandles.length; i++) {
      if (i === 0 || formattedCandles[i].time > dedupedCandles[dedupedCandles.length - 1].time) {
        dedupedCandles.push(formattedCandles[i]);
        dedupedVolumes.push(formattedVolumes[i]);
      }
    }

    if (dedupedCandles.length > 0) {
      this.lastBars = dedupedCandles;
      this.currentBar = dedupedCandles[dedupedCandles.length - 1];
      this.candlestickSeries.setData(dedupedCandles);
      this.volumeSeries.setData(dedupedVolumes);
      this.chart.timeScale().fitContent();
    }
  }

  updateLiveMarkPrice(priceNum) {
    if (!this.currentBar || isNaN(priceNum) || priceNum <= 0) return;

    // Update forming candle
    const updatedBar = {
      time: this.currentBar.time,
      open: this.currentBar.open,
      high: Math.max(this.currentBar.high, priceNum),
      low: Math.min(this.currentBar.low, priceNum),
      close: priceNum,
    };

    this.currentBar = updatedBar;
    this.candlestickSeries.update(updatedBar);
  }

  onCandleSnapshot(candle) {
    if (!candle) return;
    const rawTs = parseInt(candle.open_time_ms || candle.timestamp || candle.time, 10);
    const timeInSec = Math.floor(rawTs > 1e11 ? rawTs / 1000 : rawTs);
    const open = parseFloat(candle.open);
    const high = parseFloat(candle.high);
    const low = parseFloat(candle.low);
    const close = parseFloat(candle.close);
    const vol = parseFloat(candle.volume || '0');

    if (isNaN(timeInSec) || isNaN(open)) return;

    const bar = {
      time: timeInSec,
      open: open,
      high: high,
      low: low,
      close: close,
    };

    this.currentBar = bar;
    this.candlestickSeries.update(bar);
    this.volumeSeries.update({
      time: timeInSec,
      value: vol,
      color: close >= open ? 'rgba(16, 185, 129, 0.35)' : 'rgba(239, 68, 68, 0.35)',
    });
  }

  startPolling() {
    // Refresh candles every 15s to keep historical cache and closed bars in sync
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = setInterval(() => {
      this.loadTimeframe(this.activeInterval);
    }, 15000);
  }
}

window.BtcMarketChart = BtcMarketChart;
