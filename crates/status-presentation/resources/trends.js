document.addEventListener('DOMContentLoaded', function () {
  var holder = document.getElementById('disk-usage-chart');
  if (!holder) return;
  var url = holder.getAttribute('data-trends-json-url') || window.TRENDS_JSON_URL || 'trends.json';
  fetch(url)
    .then(function (r) {
      if (!r.ok) throw new Error('trends unavailable');
      return r.json();
    })
    .then(function (data) {
      var metric = data.external_metrics && data.external_metrics.stratum1_disk_usage;
      if (!metric) {
        holder.innerHTML = '<p class="muted">Disk usage unavailable.</p>';
        return;
      }
      renderChart(holder, metric, data.external_metrics.fallback_from_history);
    })
    .catch(function () {
      holder.innerHTML = '<p class="muted">Disk usage unavailable.</p>';
    });
});

function renderChart(holder, metric, fallback) {
  var series = metric.series || [];
  if (series.length === 0) {
    holder.innerHTML = '<p class="muted">No disk usage samples.</p>';
    return;
  }
  if (!window.Chart) {
    holder.innerHTML = '<p class="muted">Chart library unavailable.</p>';
    return;
  }

  var chartId = 'disk-usage-canvas';
  holder.innerHTML =
    '<div class="chart-summary"><strong>' + escapeHtml(metric.current_human) + '</strong><span>52w max ' + escapeHtml(metric.max_human_52w) + '</span></div>' +
    '<div class="chart-canvas-wrap"><canvas id="' + chartId + '"></canvas></div>';

  var historical = series.map(function (point) {
    return { x: point.t * 1000, y: point.bytes };
  });
  var minX = historical[0].x;
  var maxX = historical[historical.length - 1].x;
  var datasets = [{
    label: 'Used disk space',
    data: historical,
    borderColor: '#4e79a7',
    backgroundColor: 'rgba(78, 121, 167, 0.12)',
    pointBackgroundColor: '#4e79a7',
    pointBorderColor: '#ffffff',
    pointRadius: 2.5,
    pointHoverRadius: 5,
    borderWidth: 2,
    tension: 0.25,
    fill: true
  }];

  var ctx = document.getElementById(chartId).getContext('2d');
  var chart = new Chart(ctx, {
    type: 'line',
    data: { datasets: datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: 'nearest', intersect: false },
      plugins: {
        legend: {
          labels: { usePointStyle: true, boxWidth: 8 }
        },
        tooltip: {
          callbacks: {
            title: function (items) {
              if (!items.length) return '';
              return formatDate(items[0].parsed.x);
            },
            label: function (item) {
              return item.dataset.label + ': ' + formatBytes(item.parsed.y);
            }
          }
        }
      },
      scales: {
        x: {
          type: 'linear',
          min: minX,
          max: maxX,
          offset: false,
          bounds: 'data',
          ticks: {
            maxTicksLimit: 10,
            callback: function (value) { return formatShortDate(value); }
          },
          grid: { color: 'rgba(180, 187, 196, 0.35)' }
        },
        y: {
          ticks: {
            callback: function (value) { return formatTb(value); }
          },
          grid: { color: 'rgba(180, 187, 196, 0.35)' },
          title: { display: true, text: 'Used disk space' }
        }
      }
    }
  });

}

function formatBytes(bytes) {
  var tb = 1000000000000;
  var gb = 1000000000;
  if (bytes >= tb) return (bytes / tb).toFixed(2) + ' TB';
  if (bytes >= gb) return (bytes / gb).toFixed(1) + ' GB';
  return Math.round(bytes) + ' B';
}

function formatTb(bytes) {
  return (bytes / 1000000000000).toFixed(1) + ' TB';
}

function formatDate(ms) {
  return new Date(ms).toISOString().slice(0, 10);
}

function formatShortDate(ms) {
  return new Date(ms).toISOString().slice(0, 10);
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, function (ch) {
    return ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;' })[ch];
  });
}
