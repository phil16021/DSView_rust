/*
 * This file is part of the DSView project.
 * DSView is based on PulseView.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin St, Fifth Floor, Boston, MA  02110-1301 USA
 */

#include "controlapi.h"

#include <QApplication>
#include <QCoreApplication>
#include <QFileInfo>
#include <QJsonDocument>
#include <QMessageBox>
#include <QWidget>
#include <algorithm>
#include <cmath>
#include <libsigrokdecode.h>

#include "../mainwindow.h"
#include "../sigsession.h"
#include "../storesession.h"
#include "../deviceagent.h"
#include "../data/logicsnapshot.h"
#include "../data/dsosnapshot.h"
#include "../data/decoderstack.h"
#include "../data/decode/decoder.h"
#include "../data/decode/annotation.h"
#include "../data/decode/row.h"
#include "../view/view.h"
#include "../view/logicsignal.h"
#include "../view/dsosignal.h"
#include "../view/decodetrace.h"
#include "../view/dsldial.h"
#include "../toolbars/samplingbar.h"
#include "../dock/protocoldock.h"
#include "../dock/triggerdock.h"
#include "../log.h"
#ifdef DSVIEW_RUST_CORE
#include "dsview_ffi.h"
#endif

namespace pv {
namespace mcp {

namespace {

const int PollIntervalMs = 100;
const int CaptureStartGraceMs = 5000;
const uint64_t MaxEdges = 100000;
const uint64_t MaxPoints = 20000;
const uint64_t MaxAnnotations = 5000;

QJsonValue num(uint64_t v)
{
    return QJsonValue((double)v);
}

QString mode_name(int mode)
{
    switch (mode) {
    case LOGIC:  return "logic";
    case DSO:    return "dso";
    case ANALOG: return "analog";
    }
    return "unknown";
}

QString channel_type_name(int type)
{
    switch (type) {
    case SR_CHANNEL_LOGIC:  return "logic";
    case SR_CHANNEL_DSO:    return "dso";
    case SR_CHANNEL_ANALOG: return "analog";
    }
    return "other";
}

// Reads an optional unsigned integer parameter; false when present but invalid.
bool opt_u64(const QJsonObject &p, const char *key, uint64_t &out)
{
    if (!p.contains(key) || p[key].isNull())
        return true;
    double v = p[key].toDouble(-1);
    if (!p[key].isDouble() || v < 0 || v != std::floor(v) || v > 9007199254740992.0)
        return false;
    out = (uint64_t)v;
    return true;
}

bool req_int(const QJsonObject &p, const char *key, int &out)
{
    if (!p.contains(key) || !p[key].isDouble())
        return false;
    double v = p[key].toDouble();
    if (v != std::floor(v) || v < -2147483648.0 || v > 2147483647.0)
        return false;
    out = (int)v;
    return true;
}

QJsonValue variant_to_json(GVariant *v)
{
    if (v == NULL)
        return QJsonValue();
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_STRING))
        return QString::fromUtf8(g_variant_get_string(v, NULL));
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_DOUBLE))
        return g_variant_get_double(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_BOOLEAN))
        return (bool)g_variant_get_boolean(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_BYTE))
        return (int)g_variant_get_byte(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_INT16))
        return (int)g_variant_get_int16(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_UINT16))
        return (int)g_variant_get_uint16(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_INT32))
        return (int)g_variant_get_int32(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_UINT32))
        return (double)g_variant_get_uint32(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_INT64))
        return (double)g_variant_get_int64(v);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_UINT64))
        return (double)g_variant_get_uint64(v);
    return QString::fromUtf8(g_variant_print(v, FALSE));
}

QJsonArray string_list(const GSList *l)
{
    QJsonArray a;
    for (; l; l = l->next)
        a.append(QString::fromUtf8((const char *)l->data));
    return a;
}

const srd_decoder* find_decoder(const QString &id)
{
    for (const GSList *l = srd_decoder_list(); l; l = l->next) {
        const srd_decoder *d = (const srd_decoder *)l->data;
        if (id == QString::fromUtf8(d->id))
            return d;
    }
    return NULL;
}

QString check_options(const srd_decoder *d, const QJsonObject &options)
{
    for (auto it = options.begin(); it != options.end(); ++it) {
        const srd_decoder_option *found = NULL;
        for (const GSList *l = d->options; l; l = l->next) {
            const srd_decoder_option *o = (const srd_decoder_option *)l->data;
            if (it.key() == QString::fromUtf8(o->id)) {
                found = o;
                break;
            }
        }
        if (found == NULL)
            return QString("decoder '%1' has no option '%2'").arg(d->id, it.key());

        if (found->values == NULL)
            continue;

        QJsonArray allowed;
        bool ok = false;
        for (const GSList *l = found->values; l; l = l->next) {
            QJsonValue v = variant_to_json((GVariant *)l->data);
            allowed.append(v);
            if (v == it.value() || v.toVariant().toString() == it.value().toVariant().toString())
                ok = true;
        }
        if (!ok)
            return QString("option '%1' must be one of %2").arg(it.key(),
                QString::fromUtf8(QJsonDocument(allowed).toJson(QJsonDocument::Compact)));
    }
    return QString();
}

const struct { int type; const char *name; } DsoMeasures[] = {
    {DSO_MS_FREQ, "frequency"}, {DSO_MS_PERD, "period"}, {DSO_MS_PDUT, "positive_duty"},
    {DSO_MS_NDUT, "negative_duty"}, {DSO_MS_PCNT, "pulse_count"}, {DSO_MS_RISE, "rise_time"},
    {DSO_MS_FALL, "fall_time"}, {DSO_MS_PWDT, "positive_width"}, {DSO_MS_NWDT, "negative_width"},
    {DSO_MS_BRST, "burst_width"}, {DSO_MS_AMPT, "amplitude"}, {DSO_MS_VHIG, "high"},
    {DSO_MS_VLOW, "low"}, {DSO_MS_VRMS, "rms"}, {DSO_MS_VMEA, "mean"}, {DSO_MS_VP2P, "peak_to_peak"},
    {DSO_MS_VMAX, "max"}, {DSO_MS_VMIN, "min"}, {DSO_MS_POVR, "positive_overshoot"},
    {DSO_MS_NOVR, "negative_overshoot"},
};

// Describes the open modal dialog (if any) so the agent can tell the user what to close.
QString modal_dialog_text()
{
    QWidget *modal = QApplication::activeModalWidget();
    if (modal == NULL)
        return QString();

    QString text = modal->windowTitle();
    QMessageBox *box = qobject_cast<QMessageBox *>(modal);
    if (box == NULL)
        box = modal->findChild<QMessageBox *>();
    if (box != NULL && !box->text().isEmpty())
        text = text.isEmpty() ? box->text() : text + ": " + box->text();
    return text.isEmpty() ? QString("(untitled dialog)") : text.simplified().left(300);
}

} // namespace

ControlApi::ControlApi(MainWindow *window, QObject *parent) :
    QObject(parent),
    _window(window),
    _session(window->_session)
{
    _handlers = {
        {"list_devices", &ControlApi::list_devices},
        {"select_device", &ControlApi::select_device},
        {"get_config", &ControlApi::get_config},
        {"set_mode", &ControlApi::set_mode},
        {"set_sampling", &ControlApi::set_sampling},
        {"set_channels", &ControlApi::set_channels},
        {"set_trigger", &ControlApi::set_trigger},
        {"set_threshold", &ControlApi::set_threshold},
        {"set_dso", &ControlApi::set_dso},
        {"start_capture", &ControlApi::start_capture},
        {"stop_capture", &ControlApi::stop_capture},
        {"capture_status", &ControlApi::capture_status},
        {"capture_info", &ControlApi::capture_info},
        {"get_edges", &ControlApi::get_edges},
        {"read_logic", &ControlApi::read_logic},
        {"read_waveform", &ControlApi::read_waveform},
        {"list_decoders", &ControlApi::list_decoders},
        {"describe_decoder", &ControlApi::describe_decoder},
        {"add_decoder", &ControlApi::add_decoder},
        {"list_active_decoders", &ControlApi::list_active_decoders},
        {"remove_decoder", &ControlApi::remove_decoder},
        {"run_decoders", &ControlApi::run_decoders},
        {"get_annotations", &ControlApi::get_annotations},
        {"open_file", &ControlApi::open_file},
        {"save_session", &ControlApi::save_session},
        {"export_data", &ControlApi::export_data},
        {"measure_logic", &ControlApi::measure_logic},
        {"measure_dso", &ControlApi::measure_dso},
        {"show_region", &ControlApi::show_region},
        {"set_cursors", &ControlApi::set_cursors},
        {"version", &ControlApi::version},
    };

    // Methods that change state are refused while a modal dialog is open.
    _mutating = {"select_device", "set_mode", "set_sampling", "set_channels", "set_trigger",
                 "set_threshold", "set_dso", "start_capture", "add_decoder", "remove_decoder",
                 "run_decoders", "open_file", "save_session", "export_data", "show_region",
                 "set_cursors"};

    _clock.start();
    _poll_timer.setInterval(PollIntervalMs);
    connect(&_poll_timer, &QTimer::timeout, this, &ControlApi::poll);

    _session->add_msg_listener(this);
}

ControlApi::~ControlApi()
{
    _session->remove_msg_listener(this);
    _poll_timer.stop();

    for (auto &job : _store_jobs) {
        job.store->wait();
        _session->set_saving(false);
        delete job.store;
    }
}

void ControlApi::dispatch(const QString &method, const QJsonObject &params, const Reply &reply)
{
    auto it = _handlers.find(method);
    if (it == _handlers.end()) {
        reply(QJsonValue(), "unknown method: " + method);
        return;
    }

    bool mutating = std::find(_mutating.begin(), _mutating.end(), method) != _mutating.end();
    if (mutating && QApplication::activeModalWidget() != NULL) {
        reply(QJsonValue(), "DSView is showing a dialog (" + modal_dialog_text() +
                            "); ask the user to close it in the DSView window, then retry");
        return;
    }

    (this->*(it->second))(params, reply);
}

void ControlApi::OnMessage(int msg)
{
    if (msg == DSV_MSG_START_COLLECT_WORK) {
        for (auto &w : _capture_waiters)
            w.started = true;
    }
    else if (msg == DSV_MSG_END_COLLECT_WORK) {
        // Only waiters whose capture actually began; others keep waiting for their own.
        std::vector<CaptureWaiter> done;
        for (auto it = _capture_waiters.begin(); it != _capture_waiters.end();) {
            if (it->started) {
                done.push_back(*it);
                it = _capture_waiters.erase(it);
            }
            else {
                ++it;
            }
        }
        if (!done.empty()) {
            // Let the session finish its end-of-capture bookkeeping (it queues the
            // decoders) first; then answer once the automatic decoding is done too.
            QTimer::singleShot(0, this, [this, done]() {
                if (!_session->is_decoding()) {
                    QJsonObject info = capture_info_json();
                    for (auto &w : done)
                        w.reply(info, QString());
                    return;
                }
                DecodeWaiter dw;
                dw.deadline = _clock.elapsed() + 120000;
                for (int i = 0; i < (int)_session->get_decode_signals().size(); i++)
                    dw.indexes.push_back(i);
                dw.reply = [this, done](const QJsonValue &result, const QString &error) {
                    QJsonObject info = capture_info_json();
                    if (error.isEmpty())
                        info["decoders"] = result.toObject()["decoders"];
                    else
                        info["decode_warning"] = error;
                    for (auto &w : done)
                        w.reply(info, QString());
                };
                _decode_waiters.push_back(dw);
                ensure_polling();
            });
        }
    }
    else if (msg == DSV_MSG_DECODE_DONE) {
        poll();
    }
}

//------------------------------------------------------------ helpers

QString ControlApi::check_idle()
{
    if (!_session->get_device()->have_instance())
        return "no device is selected";
    if (_session->is_working())
        return "a capture is running; call stop_capture or wait for it";
    if (_session->is_saving())
        return "DSView is saving a file";
    if (_session->is_doing_action())
        return "DSView is busy";
    return QString();
}

QString ControlApi::check_logic_data(data::LogicSnapshot *&snapshot)
{
    if (_session->get_device()->get_work_mode() != LOGIC)
        return "this needs logic analyzer mode (set_mode logic)";
    snapshot = (data::LogicSnapshot *)_session->get_snapshot(SR_CHANNEL_LOGIC);
    if (snapshot == NULL || snapshot->empty() || !snapshot->have_data())
        return "no captured logic data; run start_capture first";
    return QString();
}

view::DsoSignal* ControlApi::find_dso_signal(int channel)
{
    for (auto s : _session->get_signals()) {
        if (s->signal_type() == SR_CHANNEL_DSO && s->get_index() == channel)
            return (view::DsoSignal *)s;
    }
    return NULL;
}

void ControlApi::walk_edges(data::LogicSnapshot *snapshot, int channel, uint64_t start, uint64_t end,
                            uint64_t limit, bool &initial, std::vector<uint64_t> &edges, bool &truncated)
{
    const uint64_t sample_count = snapshot->get_ring_sample_count();
    uint64_t index = start;
    bool sample = snapshot->get_sample(start, channel);

    initial = sample;
    truncated = false;

    while (index <= end) {
        if (!snapshot->get_nxt_edge(index, sample, sample_count, 1, channel))
            break;
        if (index > end)
            break;
        if (edges.size() >= limit) {
            truncated = true;
            break;
        }
        edges.push_back(index);
        sample = !sample;
    }
}

QJsonObject ControlApi::config_json()
{
    DeviceAgent *dev = _session->get_device();
    QJsonObject o;

    if (!dev->have_instance()) {
        o["device"] = QJsonValue();
        return o;
    }

    int mode = dev->get_work_mode();
    QJsonObject d;
    d["name"] = dev->name();
    d["driver"] = dev->driver_name();
    d["kind"] = dev->is_hardware() ? "hardware" : dev->is_demo() ? "demo" : dev->is_file() ? "file" : "other";
    o["device"] = d;
    o["mode"] = mode_name(mode);

    QJsonArray modes;
    for (const GSList *l = dev->get_device_mode_list(); l; l = l->next)
        modes.append(mode_name(((const sr_dev_mode *)l->data)->mode));
    o["available_modes"] = modes;

    o["samplerate"] = num(dev->get_sample_rate());
    if (mode != DSO)
        o["samples"] = num(dev->get_sample_limit());

    uint64_t depth = 0;
    if (dev->get_config_uint64(SR_CONF_HW_DEPTH, depth))
        o["hardware_depth"] = num(depth);

    QJsonArray rates;
    GVariant *gvar = dev->get_config_list(NULL, SR_CONF_SAMPLERATE);
    if (gvar != NULL) {
        GVariant *list = g_variant_lookup_value(gvar, "samplerates", G_VARIANT_TYPE("at"));
        if (list != NULL) {
            gsize n = 0;
            const uint64_t *elements = (const uint64_t *)g_variant_get_fixed_array(list, &n, sizeof(uint64_t));
            for (gsize i = 0; i < n; i++)
                rates.append(num(elements[i]));
            g_variant_unref(list);
        }
        g_variant_unref(gvar);
    }
    o["available_samplerates"] = rates;

    if (mode == DSO) {
        uint64_t tb = 0;
        if (dev->get_config_uint64(SR_CONF_TIMEBASE, tb))
            o["timebase_ns_per_div"] = num(tb);
    }

    double vth = 0;
    if (mode == LOGIC && dev->get_config_double(SR_CONF_VTH, vth))
        o["threshold_volts"] = vth;

    QJsonArray channels;
    for (const GSList *l = dev->get_channels(); l; l = l->next) {
        const sr_channel *ch = (const sr_channel *)l->data;
        QJsonObject c;
        c["index"] = ch->index;
        c["name"] = QString::fromUtf8(ch->name ? ch->name : "");
        c["enabled"] = (bool)ch->enabled;
        c["type"] = channel_type_name(ch->type);
        if (ch->type == SR_CHANNEL_DSO) {
            c["vdiv_mv"] = num(ch->vdiv);
            c["coupling"] = ch->coupling == SR_AC_COUPLING ? "ac" : ch->coupling == SR_GND_COUPLING ? "gnd" : "dc";
        }
        channels.append(c);
    }
    o["channels"] = channels;

    if (mode == LOGIC) {
        static const char *trig_names[] = {"none", "rising", "high", "falling", "low", "edge"};
        QJsonArray triggers;
        for (auto s : _session->get_signals()) {
            if (s->signal_type() != SR_CHANNEL_LOGIC)
                continue;
            int t = ((view::LogicSignal *)s)->get_trig();
            if (t > 0 && t <= 5) {
                QJsonObject tr;
                tr["channel"] = s->get_index();
                tr["type"] = trig_names[t];
                triggers.append(tr);
            }
        }
        o["triggers"] = triggers;
        o["trigger_position_percent"] = _window->_trigger_widget->get_session()["triggerPos"].toInt();
    }
    return o;
}

QJsonObject ControlApi::capture_info_json()
{
    QJsonObject o;
    DeviceAgent *dev = _session->get_device();
    if (!dev->have_instance())
        return o;

    int mode = dev->get_work_mode();
    int type = mode == DSO ? SR_CHANNEL_DSO : mode == ANALOG ? SR_CHANNEL_ANALOG : SR_CHANNEL_LOGIC;
    data::Snapshot *snapshot = _session->get_snapshot(type);
    bool have = snapshot != NULL && !snapshot->empty() && snapshot->have_data();

    o["mode"] = mode_name(mode);
    o["have_data"] = have;
    if (!have)
        return o;

    // Only the logic snapshot keeps a ring counter.
    uint64_t count = mode == LOGIC ? snapshot->get_ring_sample_count() : snapshot->get_sample_count();
    double rate = snapshot->samplerate();
    o["samples"] = num(count);
    o["samplerate"] = rate;
    if (rate > 0)
        o["duration_s"] = count / rate;
    o["triggered"] = _session->is_triged();
    o["trigger_sample"] = num(_session->get_trigger_pos());
    o["decoding"] = _session->is_decoding();

    if (_session->get_error() != SigSession::No_err)
        o["error"] = (int)_session->get_error();

    QJsonArray chans;
    for (auto s : _session->get_signals()) {
        if (s->signal_type() == type && snapshot->has_data(s->get_index())) {
            QJsonObject c;
            c["index"] = s->get_index();
            c["name"] = s->get_name();
            chans.append(c);
        }
    }
    o["channels"] = chans;
    return o;
}

QJsonObject ControlApi::decoder_state_json(int index)
{
    auto trace = _session->get_decode_signals()[index];
    auto stack = trace->decoder();
    QJsonObject o;
    o["index"] = index;
    o["name"] = trace->get_name();

    QJsonArray ids;
    for (auto dec : stack->stack())
        ids.append(QString::fromUtf8(dec->decoder()->id));
    o["stack"] = ids;
    o["progress"] = stack->get_progress();
    o["annotations"] = num(stack->get_result_count());
    if (!stack->error_message().isEmpty())
        o["error"] = stack->error_message();

    QJsonArray rows;
    auto shown = stack->get_rows_gshow();
    for (auto it = shown.begin(); it != shown.end(); ++it) {
        QJsonObject r;
        r["title"] = it->first.title();
        if (it->first.row() != NULL)
            r["id"] = QString::fromUtf8(it->first.row()->id);
        rows.append(r);
    }
    o["rows"] = rows;
    return o;
}

void ControlApi::ensure_polling()
{
    if (!_poll_timer.isActive())
        _poll_timer.start();
}

void ControlApi::wait_capture(const Reply &r, double timeout_s)
{
    qint64 now = _clock.elapsed();
    CaptureWaiter w;
    w.reply = r;
    w.deadline = now + (qint64)(timeout_s * 1000);
    w.start_by = now + CaptureStartGraceMs;
    w.started = false;
    _capture_waiters.push_back(w);
    ensure_polling();
}

void ControlApi::poll()
{
    qint64 now = _clock.elapsed();

    for (auto it = _capture_waiters.begin(); it != _capture_waiters.end();) {
        if (!it->started && _session->is_working())
            it->started = true;

        if (!it->started && now > it->start_by) {
            Reply r = it->reply;
            it = _capture_waiters.erase(it);
            r(QJsonValue(), "the capture did not start (check the DSView window for a message)");
        }
        else if (now > it->deadline) {
            Reply r = it->reply;
            it = _capture_waiters.erase(it);
            r(QJsonValue(), "timed out waiting for the capture to finish; it may still be running "
                            "(use capture_status or stop_capture)");
        }
        else {
            ++it;
        }
    }

    for (auto it = _decode_waiters.begin(); it != _decode_waiters.end();) {
        if (!_session->is_decoding()) {
            QJsonArray states;
            int count = (int)_session->get_decode_signals().size();
            for (int i : it->indexes) {
                if (i < count)
                    states.append(decoder_state_json(i));
            }
            Reply r = it->reply;
            it = _decode_waiters.erase(it);
            QJsonObject o;
            o["decoders"] = states;
            r(o, QString());
        }
        else if (now > it->deadline) {
            Reply r = it->reply;
            it = _decode_waiters.erase(it);
            r(QJsonValue(), "timed out waiting for decoding; it is still running");
        }
        else {
            ++it;
        }
    }

    for (auto it = _store_jobs.begin(); it != _store_jobs.end();) {
        if (it->store->is_busy()) {
            ++it;
            continue;
        }
        StoreJob job = *it;
        it = _store_jobs.erase(it);

        job.store->wait();
        _session->set_saving(false);
        QString err = job.store->error();
        if (err.isEmpty() && !job.is_export)
            _session->clear_store_confirm_flag();
        delete job.store;

        QJsonObject o;
        o["path"] = job.path;
        job.reply(o, err);
    }

    if (_capture_waiters.empty() && _decode_waiters.empty() && _store_jobs.empty())
        _poll_timer.stop();
}

//------------------------------------------------------------ device + capture

void ControlApi::list_devices(const QJsonObject &, const Reply &r)
{
    int count = 0, active = -1;
    struct ds_device_base_info *list = _session->get_device_list(count, active);
    QJsonArray arr;
    for (int i = 0; list != NULL && i < count; i++) {
        QJsonObject d;
        d["index"] = i;
        d["name"] = QString::fromUtf8(list[i].name);
        d["active"] = i == active;
        arr.append(d);
    }
    if (list != NULL)
        g_free(list);
    QJsonObject o;
    o["devices"] = arr;
    r(o, QString());
}

void ControlApi::select_device(const QJsonObject &p, const Reply &r)
{
    int index;
    if (!req_int(p, "index", index)) {
        r(QJsonValue(), "parameter 'index' (from list_devices) is required");
        return;
    }
    if (_session->is_working() || _session->is_saving()) {
        r(QJsonValue(), "DSView is capturing or saving");
        return;
    }

    int count = 0, active = -1;
    struct ds_device_base_info *list = _session->get_device_list(count, active);
    if (list == NULL || index < 0 || index >= count) {
        if (list != NULL)
            g_free(list);
        r(QJsonValue(), QString("device index out of range (0..%1)").arg(count - 1));
        return;
    }
    ds_device_handle handle = list[index].handle;
    g_free(list);

    if (index != active && !_session->set_device(handle)) {
        r(QJsonValue(), "could not open the device");
        return;
    }
    r(config_json(), QString());
}

void ControlApi::get_config(const QJsonObject &, const Reply &r)
{
    r(config_json(), QString());
}

void ControlApi::set_mode(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }

    QString name = p["mode"].toString();
    int mode = name == "logic" ? LOGIC : name == "dso" ? DSO : name == "analog" ? ANALOG : -1;
    if (mode < 0) {
        r(QJsonValue(), "mode must be 'logic', 'dso' or 'analog'");
        return;
    }

    DeviceAgent *dev = _session->get_device();
    bool supported = false;
    for (const GSList *l = dev->get_device_mode_list(); l; l = l->next)
        supported |= ((const sr_dev_mode *)l->data)->mode == mode;
    if (!supported) {
        r(QJsonValue(), "this device does not support mode " + name);
        return;
    }

    if (dev->get_work_mode() != mode) {
        _session->session_save();
        _session->switch_work_mode(mode);
        _window->_view->set_device();
    }
    r(config_json(), QString());
}

void ControlApi::set_sampling(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }

    uint64_t rate = 0, samples = 0;
    if (!opt_u64(p, "samplerate", rate) || !opt_u64(p, "samples", samples)) {
        r(QJsonValue(), "samplerate and samples must be non-negative integers");
        return;
    }

    DeviceAgent *dev = _session->get_device();
    if (rate != 0) {
        QJsonArray allowed = config_json()["available_samplerates"].toArray();
        if (!allowed.isEmpty() && !allowed.contains(num(rate))) {
            r(QJsonValue(), "unsupported samplerate; see available_samplerates in get_config");
            return;
        }
        dev->set_config_uint64(SR_CONF_SAMPLERATE, rate);
    }
    if (samples != 0) {
        if (dev->get_work_mode() == DSO) {
            r(QJsonValue(), "in dso mode use set_dso timebase_ns instead of samples");
            return;
        }
        dev->set_config_uint64(SR_CONF_LIMIT_SAMPLES, samples);
    }

    // Same order as loading a profile: refresh the selectors, then let them
    // push the (rounded) values back to the device.
    _window->_sampling_bar->update_sample_rate_list();
    _window->_sampling_bar->commit_settings();
    _session->broadcast_msg(DSV_MSG_DEVICE_DURATION_UPDATED);

    QJsonObject o;
    o["samplerate"] = num(dev->get_sample_rate());
    if (dev->get_work_mode() != DSO)
        o["samples"] = num(dev->get_sample_limit());
    o["note"] = "sample counts snap to the depth steps DSView offers";
    r(o, QString());
}

void ControlApi::set_channels(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    if (!p["channels"].isArray()) {
        r(QJsonValue(), "parameter 'channels' must be an array of {index, enabled?, name?}");
        return;
    }

    DeviceAgent *dev = _session->get_device();
    int mode = dev->get_work_mode();
    std::vector<const sr_channel *> chans;
    for (const GSList *l = dev->get_channels(); l; l = l->next)
        chans.push_back((const sr_channel *)l->data);

    for (const QJsonValue &v : p["channels"].toArray()) {
        QJsonObject c = v.toObject();
        int index;
        if (!req_int(c, "index", index)) {
            r(QJsonValue(), "each channel needs an integer 'index'");
            return;
        }
        const sr_channel *ch = NULL;
        for (auto x : chans) {
            if (x->index == index)
                ch = x;
        }
        if (ch == NULL) {
            r(QJsonValue(), QString("channel %1 does not exist").arg(index));
            return;
        }
        if (c.contains("enabled")) {
            bool en = c["enabled"].toBool();
            if (mode == DSO)
                dev->set_config_bool(SR_CONF_PROBE_EN, en, ch, NULL);
            else
                dev->enable_probe(index, en);
        }
        if (c.contains("name")) {
            QString name = c["name"].toString().left(31);
            dev->set_channel_name(index, name.toUtf8().constData());
        }
    }

    _session->broadcast_msg(DSV_MSG_DEVICE_OPTIONS_UPDATED);
    r(config_json(), QString());
}

void ControlApi::set_trigger(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    if (_session->get_device()->get_work_mode() != LOGIC) {
        r(QJsonValue(), "set_trigger supports logic mode; DSO triggers are set in the GUI");
        return;
    }

    QString type = p["type"].toString("none");
    int trig = type == "none" ? view::LogicSignal::NONTRIG :
               type == "rising" ? view::LogicSignal::POSTRIG :
               type == "high" ? view::LogicSignal::HIGTRIG :
               type == "falling" ? view::LogicSignal::NEGTRIG :
               type == "low" ? view::LogicSignal::LOWTRIG :
               type == "edge" ? view::LogicSignal::EDGTRIG : -1;
    if (trig < 0) {
        r(QJsonValue(), "type must be none, rising, falling, high, low or edge");
        return;
    }

    int channel = -1;
    if (trig != view::LogicSignal::NONTRIG && !req_int(p, "channel", channel)) {
        r(QJsonValue(), "parameter 'channel' is required for a trigger");
        return;
    }

    bool found = trig == view::LogicSignal::NONTRIG;
    for (auto s : _session->get_signals()) {
        if (s->signal_type() != SR_CHANNEL_LOGIC)
            continue;
        auto logic = (view::LogicSignal *)s;
        bool target = s->get_index() == channel;
        found |= target;
        logic->set_trig(target ? trig : (int)view::LogicSignal::NONTRIG);
    }
    if (!found) {
        r(QJsonValue(), QString("logic channel %1 is not shown").arg(channel));
        return;
    }

    QJsonObject ses = _window->_trigger_widget->get_session();
    ses["advTriggerMode"] = false;
    if (p.contains("position_percent")) {
        int pos;
        if (!req_int(p, "position_percent", pos) || pos < 0 || pos > 100) {
            r(QJsonValue(), "position_percent must be 0..100");
            return;
        }
        ses["triggerPos"] = pos;
    }
    _window->_trigger_widget->set_session(ses);
    _window->_view->data_updated();
    r(config_json(), QString());
}

void ControlApi::set_threshold(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    if (!p["volts"].isDouble()) {
        r(QJsonValue(), "parameter 'volts' (number) is required");
        return;
    }
    DeviceAgent *dev = _session->get_device();
    double old;
    if (!dev->get_config_double(SR_CONF_VTH, old)) {
        r(QJsonValue(), "this device has no adjustable threshold voltage");
        return;
    }
    double v = p["volts"].toDouble();
    if (v < 0 || v > 5) {
        r(QJsonValue(), "volts must be between 0 and 5");
        return;
    }
    dev->set_config_double(SR_CONF_VTH, v);
    _session->broadcast_msg(DSV_MSG_DEVICE_OPTIONS_UPDATED);
    r(config_json(), QString());
}

void ControlApi::set_dso(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    DeviceAgent *dev = _session->get_device();
    if (dev->get_work_mode() != DSO) {
        r(QJsonValue(), "set_dso needs oscilloscope mode (set_mode dso)");
        return;
    }

    uint64_t timebase = 0, vdiv = 0;
    if (!opt_u64(p, "timebase_ns", timebase) || !opt_u64(p, "vdiv_mv", vdiv)) {
        r(QJsonValue(), "timebase_ns and vdiv_mv must be non-negative integers");
        return;
    }

    if (timebase != 0) {
        uint64_t lo = 0, hi = 0;
        dev->get_config_uint64(SR_CONF_MIN_TIMEBASE, lo);
        dev->get_config_uint64(SR_CONF_MAX_TIMEBASE, hi);
        if ((lo && timebase < lo) || (hi && timebase > hi)) {
            r(QJsonValue(), QString("timebase_ns must be within %1..%2").arg(lo).arg(hi));
            return;
        }
        dev->set_config_uint64(SR_CONF_TIMEBASE, timebase);
        _window->_sampling_bar->update_sample_rate_list();
        _window->_sampling_bar->commit_settings();
        _session->broadcast_msg(DSV_MSG_DEVICE_DURATION_UPDATED);
    }

    if (vdiv != 0 || p.contains("coupling")) {
        int channel;
        if (!req_int(p, "channel", channel)) {
            r(QJsonValue(), "parameter 'channel' is required for vdiv_mv/coupling");
            return;
        }
        view::DsoSignal *sig = find_dso_signal(channel);
        if (sig == NULL || !sig->enabled()) {
            r(QJsonValue(), QString("dso channel %1 is not enabled").arg(channel));
            return;
        }

        // Step the V/div dial like the GUI knob, toward the requested value.
        for (int i = 0; vdiv != 0 && i < 64; i++) {
            uint64_t cur = sig->get_vDialValue();
            if (cur == vdiv)
                break;
            bool moved = cur < vdiv ? sig->go_vDialNext(true) : sig->go_vDialPre(true);
            uint64_t now = sig->get_vDialValue();
            if (!moved || (cur < vdiv && now > vdiv) || (cur > vdiv && now < vdiv))
                break;
        }

        if (p.contains("coupling")) {
            QString c = p["coupling"].toString();
            int cp = c == "dc" ? SR_DC_COUPLING : c == "ac" ? SR_AC_COUPLING : c == "gnd" ? SR_GND_COUPLING : -1;
            if (cp < 0) {
                r(QJsonValue(), "coupling must be dc, ac or gnd");
                return;
            }
            sig->set_acCoupling((uint8_t)cp);
        }
        _window->_view->data_updated();
    }
    r(config_json(), QString());
}

void ControlApi::start_capture(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    DeviceAgent *dev = _session->get_device();
    if (!dev->have_enabled_channel()) {
        r(QJsonValue(), "no channel is enabled; use set_channels");
        return;
    }

    // DSO captures run until stopped unless they are single-shot.
    bool instant = p.contains("instant") ? p["instant"].toBool() : dev->get_work_mode() == DSO;
    bool wait = p.contains("wait") ? p["wait"].toBool() : true;
    double timeout = p.contains("timeout_s") ? p["timeout_s"].toDouble(60) : 60;
    timeout = std::max(1.0, std::min(timeout, 3600.0));

    if (wait)
        wait_capture(r, timeout);

    if (instant)
        _window->_sampling_bar->run_or_stop_instant();
    else
        _window->_sampling_bar->run_or_stop();

    if (!wait) {
        QJsonObject o;
        o["started"] = true;
        r(o, QString());
    }
}

void ControlApi::stop_capture(const QJsonObject &, const Reply &r)
{
    if (_session->is_working())
        _session->stop_capture();
    QJsonObject o;
    o["working"] = _session->is_working();
    r(o, QString());
}

void ControlApi::capture_status(const QJsonObject &, const Reply &r)
{
    QJsonObject o;
    bool triggered = false;
    int progress = 0;
    o["working"] = _session->is_working();
    if (_session->is_working() && _session->get_capture_status(triggered, progress)) {
        o["triggered"] = triggered;
        o["progress_percent"] = progress;
    }
    o["decoding"] = _session->is_decoding();
    o["saving"] = _session->is_saving();
    if (QApplication::activeModalWidget() != NULL)
        o["dialog"] = modal_dialog_text();
    o["capture"] = capture_info_json();
    r(o, QString());
}

//------------------------------------------------------------ readout

void ControlApi::capture_info(const QJsonObject &, const Reply &r)
{
    r(capture_info_json(), QString());
}

void ControlApi::get_edges(const QJsonObject &p, const Reply &r)
{
    data::LogicSnapshot *snapshot = NULL;
    QString err = check_logic_data(snapshot);
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }

    int channel;
    if (!req_int(p, "channel", channel) || !snapshot->has_data(channel)) {
        r(QJsonValue(), "parameter 'channel' must be a captured logic channel index");
        return;
    }

    uint64_t count = snapshot->get_ring_sample_count();
    uint64_t start = 0, end = count - 1, limit = 1000;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end) || !opt_u64(p, "limit", limit)) {
        r(QJsonValue(), "start, end and limit must be non-negative integers");
        return;
    }
    end = std::min(end, count - 1);
    if (start > end) {
        r(QJsonValue(), "start must not be greater than end");
        return;
    }
    limit = std::max<uint64_t>(1, std::min(limit, MaxEdges));

    bool initial, truncated;
    std::vector<uint64_t> edges;
    walk_edges(snapshot, channel, start, end, limit, initial, edges, truncated);

    double rate = snapshot->samplerate();
    QJsonArray arr;
    bool level = initial;
    for (uint64_t e : edges) {
        level = !level;
        QJsonObject o;
        o["sample"] = num(e);
        o["time_ns"] = rate > 0 ? e * 1e9 / rate : 0;
        o["edge"] = level ? "rising" : "falling";
        arr.append(o);
    }

    QJsonObject o;
    o["channel"] = channel;
    o["samplerate"] = rate;
    o["start"] = num(start);
    o["end"] = num(end);
    o["initial_level"] = initial ? 1 : 0;
    o["edges"] = arr;
    o["truncated"] = truncated;
    if (truncated && !edges.empty())
        o["next_start"] = num(edges.back() + 1);
    r(o, QString());
}

void ControlApi::read_logic(const QJsonObject &p, const Reply &r)
{
    data::LogicSnapshot *snapshot = NULL;
    QString err = check_logic_data(snapshot);
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }

    uint64_t count = snapshot->get_ring_sample_count();
    uint64_t start = 0, end = count - 1, limit = 2000;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end) || !opt_u64(p, "max_transitions", limit)) {
        r(QJsonValue(), "start, end and max_transitions must be non-negative integers");
        return;
    }
    end = std::min(end, count - 1);
    if (start > end) {
        r(QJsonValue(), "start must not be greater than end");
        return;
    }
    limit = std::max<uint64_t>(1, std::min(limit, MaxEdges));

    std::vector<int> channels;
    if (p["channels"].isArray()) {
        for (const QJsonValue &v : p["channels"].toArray())
            channels.push_back(v.toInt(-1));
    }
    else {
        for (auto s : _session->get_signals()) {
            if (s->signal_type() == SR_CHANNEL_LOGIC && snapshot->has_data(s->get_index()))
                channels.push_back(s->get_index());
        }
    }

    QJsonObject out;
    for (int ch : channels) {
        if (ch < 0 || !snapshot->has_data(ch)) {
            r(QJsonValue(), QString("channel %1 has no captured data").arg(ch));
            return;
        }
        bool initial, truncated;
        std::vector<uint64_t> edges;
        walk_edges(snapshot, ch, start, end, limit, initial, edges, truncated);

        QJsonArray t;
        for (uint64_t e : edges)
            t.append(num(e));
        QJsonObject c;
        c["initial_level"] = initial ? 1 : 0;
        c["transitions"] = t;
        c["truncated"] = truncated;
        out[QString::number(ch)] = c;
    }

    QJsonObject o;
    o["samplerate"] = snapshot->samplerate();
    o["start"] = num(start);
    o["end"] = num(end);
    o["channels"] = out;
    o["format"] = "each channel starts at initial_level at 'start' and toggles at every sample in transitions";
    r(o, QString());
}

void ControlApi::read_waveform(const QJsonObject &p, const Reply &r)
{
    if (_session->get_device()->get_work_mode() != DSO) {
        r(QJsonValue(), "read_waveform supports oscilloscope (dso) mode");
        return;
    }
    int channel;
    if (!req_int(p, "channel", channel)) {
        r(QJsonValue(), "parameter 'channel' is required");
        return;
    }
    view::DsoSignal *sig = find_dso_signal(channel);
    if (sig == NULL || !sig->enabled()) {
        r(QJsonValue(), QString("dso channel %1 is not enabled").arg(channel));
        return;
    }
    data::DsoSnapshot *snapshot = sig->data();
    if (snapshot == NULL || snapshot->empty() || !snapshot->has_data(channel)) {
        r(QJsonValue(), "no captured waveform; run start_capture first");
        return;
    }

    const uint64_t n = snapshot->get_sample_count();
    const uint8_t *buf = snapshot->get_samples(0, 0, channel);
    const int height = sig->get_view_rect().height();
    view::dslDial *vdial = sig->get_vDial();
    if (n == 0 || buf == NULL || height <= 0 || vdial == NULL) {
        r(QJsonValue(), "waveform is not available yet");
        return;
    }

    // Same calibration as the on-screen trace (see SigSession::add_ref_wave).
    const int hw_offset = sig->get_hw_offset();
    const double mv_per_code = sig->get_scale() * (double)vdial->get_value()
                               * vdial->get_factor() * DS_CONF_DSO_VDIVS / height;

    uint64_t start = 0, end = n - 1, max_points = 1000;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end) || !opt_u64(p, "max_points", max_points)) {
        r(QJsonValue(), "start, end and max_points must be non-negative integers");
        return;
    }
    end = std::min(end, n - 1);
    if (start > end) {
        r(QJsonValue(), "start must not be greater than end");
        return;
    }
    max_points = std::max<uint64_t>(1, std::min(max_points, MaxPoints));

    const uint64_t span = end - start + 1;
    const uint64_t step = (span + max_points - 1) / max_points;
    QJsonArray mins, maxs;
    for (uint64_t i = start; i <= end; i += step) {
        uint8_t lo = 255, hi = 0;
        for (uint64_t j = i; j < i + step && j <= end; j++) {
            lo = std::min(lo, buf[j]);
            hi = std::max(hi, buf[j]);
        }
        // A higher code is a lower voltage.
        mins.append(std::round((hw_offset - hi) * mv_per_code * 1000) / 1000);
        maxs.append(std::round((hw_offset - lo) * mv_per_code * 1000) / 1000);
    }

    QJsonObject o;
    o["channel"] = channel;
    o["samplerate"] = snapshot->samplerate();
    o["start"] = num(start);
    o["end"] = num(end);
    o["samples_per_point"] = num(step);
    if (step == 1) {
        o["mv"] = maxs;
    }
    else {
        o["min_mv"] = mins;
        o["max_mv"] = maxs;
    }
    r(o, QString());
}

//------------------------------------------------------------ decoders

void ControlApi::list_decoders(const QJsonObject &p, const Reply &r)
{
    QString filter = p["filter"].toString().toLower();
    QJsonArray arr;
    for (const GSList *l = srd_decoder_list(); l; l = l->next) {
        const srd_decoder *d = (const srd_decoder *)l->data;
        QString id = QString::fromUtf8(d->id);
        QString name = QString::fromUtf8(d->name);
        QString longname = QString::fromUtf8(d->longname);
        QJsonArray tags = string_list(d->tags);
        if (!filter.isEmpty()) {
            QString hay = (id + " " + name + " " + longname).toLower();
            for (const QJsonValue &t : tags)
                hay += " " + t.toString().toLower();
            if (!hay.contains(filter))
                continue;
        }
        QJsonObject o;
        o["id"] = id;
        o["name"] = name;
        o["longname"] = longname;
        o["inputs"] = string_list(d->inputs);
        o["outputs"] = string_list(d->outputs);
        o["tags"] = tags;
        arr.append(o);
    }
    QJsonObject o;
    o["decoders"] = arr;
    o["note"] = "decoders with input 'logic' attach to channels; others stack on a decoder whose outputs match their inputs";
    r(o, QString());
}

void ControlApi::describe_decoder(const QJsonObject &p, const Reply &r)
{
    const srd_decoder *d = find_decoder(p["id"].toString());
    if (d == NULL) {
        r(QJsonValue(), "unknown decoder id; see list_decoders");
        return;
    }

    auto channels = [](const GSList *l) {
        QJsonArray a;
        for (; l; l = l->next) {
            const srd_channel *c = (const srd_channel *)l->data;
            QJsonObject o;
            o["id"] = QString::fromUtf8(c->id);
            o["name"] = QString::fromUtf8(c->name);
            o["desc"] = QString::fromUtf8(c->desc);
            a.append(o);
        }
        return a;
    };

    QJsonArray options;
    for (const GSList *l = d->options; l; l = l->next) {
        const srd_decoder_option *opt = (const srd_decoder_option *)l->data;
        QJsonObject o;
        o["id"] = QString::fromUtf8(opt->id);
        o["desc"] = QString::fromUtf8(opt->desc);
        o["default"] = variant_to_json(opt->def);
        if (opt->values != NULL) {
            QJsonArray values;
            for (const GSList *v = opt->values; v; v = v->next)
                values.append(variant_to_json((GVariant *)v->data));
            o["values"] = values;
        }
        options.append(o);
    }

    QJsonArray rows;
    for (const GSList *l = d->annotation_rows; l; l = l->next) {
        const srd_decoder_annotation_row *row = (const srd_decoder_annotation_row *)l->data;
        QJsonObject o;
        o["id"] = QString::fromUtf8(row->id);
        o["desc"] = QString::fromUtf8(row->desc);
        rows.append(o);
    }

    QJsonObject o;
    o["id"] = QString::fromUtf8(d->id);
    o["name"] = QString::fromUtf8(d->name);
    o["longname"] = QString::fromUtf8(d->longname);
    o["desc"] = QString::fromUtf8(d->desc);
    o["inputs"] = string_list(d->inputs);
    o["outputs"] = string_list(d->outputs);
    o["channels"] = channels(d->channels);
    o["optional_channels"] = channels(d->opt_channels);
    o["options"] = options;
    o["annotation_rows"] = rows;
    r(o, QString());
}

void ControlApi::add_decoder(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    if (_session->get_device()->get_work_mode() != LOGIC) {
        r(QJsonValue(), "protocol decoders need logic analyzer mode");
        return;
    }

    const srd_decoder *root = find_decoder(p["id"].toString());
    if (root == NULL) {
        r(QJsonValue(), "unknown decoder id; see list_decoders");
        return;
    }
    if (!string_list(root->inputs).contains(QJsonValue("logic"))) {
        r(QJsonValue(), QString("'%1' does not take logic input; stack it on another decoder via 'stack'").arg(root->id));
        return;
    }

    // Channel mapping: decoder channel id -> logic channel index.
    QJsonObject map = p["channels"].toObject();
    std::vector<int> logic_channels;
    for (const GSList *l = _session->get_device()->get_channels(); l; l = l->next)
        logic_channels.push_back(((const sr_channel *)l->data)->index);

    QJsonArray channel_arr;
    for (auto it = map.begin(); it != map.end(); ++it) {
        bool known = false;
        for (int pass = 0; pass < 2 && !known; pass++) {
            for (const GSList *l = pass == 0 ? root->channels : root->opt_channels; l; l = l->next)
                known |= it.key() == QString::fromUtf8(((const srd_channel *)l->data)->id);
        }
        if (!known) {
            r(QJsonValue(), QString("decoder '%1' has no channel '%2'").arg(root->id, it.key()));
            return;
        }
        int index = it.value().toInt(-1);
        if (std::find(logic_channels.begin(), logic_channels.end(), index) == logic_channels.end()) {
            r(QJsonValue(), QString("logic channel %1 does not exist").arg(index));
            return;
        }
        QJsonObject c;
        c[it.key()] = index;
        channel_arr.append(c);
    }
    for (const GSList *l = root->channels; l; l = l->next) {
        QString id = QString::fromUtf8(((const srd_channel *)l->data)->id);
        if (!map.contains(id)) {
            r(QJsonValue(), QString("required channel '%1' is not mapped").arg(id));
            return;
        }
    }

    QJsonObject options = p["options"].toObject();
    err = check_options(root, options);
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }

    QJsonArray stacked;
    QJsonArray prev_outputs = string_list(root->outputs);
    for (const QJsonValue &v : p["stack"].toArray()) {
        QJsonObject s = v.toObject();
        const srd_decoder *d = find_decoder(s["id"].toString());
        if (d == NULL) {
            r(QJsonValue(), "unknown stacked decoder id: " + s["id"].toString());
            return;
        }
        bool fits = false;
        for (const QJsonValue &in : string_list(d->inputs))
            fits |= prev_outputs.contains(in);
        if (!fits) {
            r(QJsonValue(), QString("'%1' cannot stack on the previous decoder (inputs don't match its outputs)").arg(d->id));
            return;
        }
        err = check_options(d, s["options"].toObject());
        if (!err.isEmpty()) {
            r(QJsonValue(), err);
            return;
        }
        QJsonObject so;
        so["id"] = QString::fromUtf8(d->id);
        so["options"] = s["options"].toObject();
        stacked.append(so);
        prev_outputs = string_list(d->outputs);
    }

    QJsonObject dec;
    dec["id"] = QString::fromUtf8(root->id);
    dec["channel"] = channel_arr;
    dec["options"] = options;
    if (!stacked.isEmpty())
        dec["stacked decoders"] = stacked;
    QJsonArray arr;
    arr.append(dec);

    size_t before = _session->get_decode_signals().size();
    StoreSession ss(_session);
    ss.load_decoders(_window->_protocol_widget, arr);
    if (_session->get_decode_signals().size() <= before) {
        r(QJsonValue(), "DSView could not add the decoder");
        return;
    }

    int index = (int)_session->get_decode_signals().size() - 1;
    bool run = p.contains("run") ? p["run"].toBool() : true;
    if (run && _session->have_view_data() && _session->start_decode(index)) {
        DecodeWaiter w;
        w.reply = r;
        w.deadline = _clock.elapsed() + 120000;
        w.indexes.push_back(index);
        _decode_waiters.push_back(w);
        ensure_polling();
        return;
    }

    QJsonObject o;
    o["decoders"] = QJsonArray({decoder_state_json(index)});
    r(o, QString());
}

void ControlApi::list_active_decoders(const QJsonObject &, const Reply &r)
{
    QJsonArray arr;
    int count = (int)_session->get_decode_signals().size();
    for (int i = 0; i < count; i++)
        arr.append(decoder_state_json(i));
    QJsonObject o;
    o["decoders"] = arr;
    o["decoding"] = _session->is_decoding();
    r(o, QString());
}

void ControlApi::remove_decoder(const QJsonObject &p, const Reply &r)
{
    int index;
    if (!req_int(p, "index", index) || index < 0 || index >= (int)_session->get_decode_signals().size()) {
        r(QJsonValue(), "parameter 'index' must be a decoder index from list_active_decoders");
        return;
    }
    if (_session->is_working()) {
        r(QJsonValue(), "a capture is running");
        return;
    }
    if (!_window->_protocol_widget->remove_protocol(index)) {
        r(QJsonValue(), "could not remove the decoder");
        return;
    }
    QJsonObject o;
    o["removed"] = index;
    r(o, QString());
}

void ControlApi::run_decoders(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    if (!_session->have_view_data()) {
        r(QJsonValue(), "no captured data to decode");
        return;
    }

    int count = (int)_session->get_decode_signals().size();
    std::vector<int> indexes;
    if (p.contains("index")) {
        int index;
        if (!req_int(p, "index", index) || index < 0 || index >= count) {
            r(QJsonValue(), "parameter 'index' must be a decoder index from list_active_decoders");
            return;
        }
        indexes.push_back(index);
    }
    else {
        for (int i = 0; i < count; i++)
            indexes.push_back(i);
    }
    if (indexes.empty()) {
        r(QJsonValue(), "no decoders are configured; use add_decoder");
        return;
    }

    for (int i : indexes)
        _session->start_decode(i);

    double timeout = p.contains("timeout_s") ? p["timeout_s"].toDouble(120) : 120;
    DecodeWaiter w;
    w.reply = r;
    w.deadline = _clock.elapsed() + (qint64)(std::max(1.0, std::min(timeout, 3600.0)) * 1000);
    w.indexes = indexes;
    _decode_waiters.push_back(w);
    ensure_polling();
}

void ControlApi::get_annotations(const QJsonObject &p, const Reply &r)
{
    int index;
    int count = (int)_session->get_decode_signals().size();
    if (!req_int(p, "decoder", index) || index < 0 || index >= count) {
        r(QJsonValue(), "parameter 'decoder' must be a decoder index from list_active_decoders");
        return;
    }
    if (_session->is_decoding()) {
        r(QJsonValue(), "decoding is still running; call run_decoders (it waits) and retry");
        return;
    }

    uint64_t start = 0, end = UINT64_MAX >> 12, offset = 0, limit = 500;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end) ||
        !opt_u64(p, "offset", offset) || !opt_u64(p, "limit", limit)) {
        r(QJsonValue(), "start, end, offset and limit must be non-negative integers");
        return;
    }
    limit = std::max<uint64_t>(1, std::min(limit, MaxAnnotations));
    QString row_filter = p["row"].toString().toLower();

    auto stack = _session->get_decode_signals()[index]->decoder();
    double rate = (double)stack->sample_rate();

    struct Item { uint64_t start, end; QString row, text; };
    std::vector<Item> items;
    auto rows = stack->get_rows_gshow();
    for (auto it = rows.begin(); it != rows.end(); ++it) {
        const auto &row = it->first;
        QString title = row.title();
        QString id = row.row() ? QString::fromUtf8(row.row()->id) : QString();
        if (!row_filter.isEmpty() && !title.toLower().contains(row_filter) && id.toLower() != row_filter)
            continue;

        std::vector<data::decode::Annotation *> anns;
        stack->get_annotation_subset(anns, row, start, end);
        for (auto a : anns) {
            const auto &texts = a->annotations();
            items.push_back({a->start_sample(), a->end_sample(), title, texts.empty() ? QString() : texts.front()});
        }
    }
    std::stable_sort(items.begin(), items.end(), [](const Item &a, const Item &b) { return a.start < b.start; });

    QJsonArray arr;
    for (uint64_t i = offset; i < items.size() && (uint64_t)arr.size() < limit; i++) {
        QJsonObject o;
        o["start_sample"] = num(items[i].start);
        o["end_sample"] = num(items[i].end);
        if (rate > 0)
            o["start_ns"] = items[i].start * 1e9 / rate;
        o["row"] = items[i].row;
        o["text"] = items[i].text;
        arr.append(o);
    }

    QJsonObject o;
    o["decoder"] = index;
    o["total"] = num(items.size());
    o["offset"] = num(offset);
    o["annotations"] = arr;
    if (offset + arr.size() < items.size())
        o["next_offset"] = num(offset + arr.size());
    r(o, QString());
}

//------------------------------------------------------------ files, measurements, view

void ControlApi::open_file(const QJsonObject &p, const Reply &r)
{
    if (_session->is_working() || _session->is_saving()) {
        r(QJsonValue(), "DSView is capturing or saving");
        return;
    }
    QString path = p["path"].toString();
    QFileInfo fi(path);
    if (path.isEmpty() || !fi.isFile() || !fi.isReadable()) {
        r(QJsonValue(), "file not found or not readable: " + path);
        return;
    }

    // Loading switches to a file device and replays it as a capture.
    wait_capture(r, 120);
    _window->on_load_file(fi.absoluteFilePath());
}

void ControlApi::start_store(StoreSession *store, const QString &path, bool is_export, const Reply &r)
{
    store->_sessionDataGetter = _window;
    store->SetDataRange(0, 0);

    bool ok = is_export ? store->export_start() : store->save_start();
    if (!ok) {
        QString err = store->error();
        delete store;
        r(QJsonValue(), err.isEmpty() ? "could not start writing the file" : err);
        return;
    }

    _session->set_saving(true);
    _store_jobs.push_back({r, store, path, is_export});
    ensure_polling();
}

void ControlApi::save_session(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    QString path = p["path"].toString();
    if (!path.endsWith(".dsl")) {
        r(QJsonValue(), "path must end with .dsl");
        return;
    }
    if (!_session->have_view_data()) {
        r(QJsonValue(), "no captured data to save");
        return;
    }
    StoreSession *store = new StoreSession(_session);
    store->SetFileName(path, "dsl");
    start_store(store, path, false, r);
}

void ControlApi::export_data(const QJsonObject &p, const Reply &r)
{
    QString err = check_idle();
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    QString path = p["path"].toString();
    QString format = p["format"].toString(QFileInfo(path).suffix()).toLower();
    if (format != "csv" && format != "vcd") {
        r(QJsonValue(), "format must be csv or vcd");
        return;
    }
    if (format == "vcd" && _session->get_device()->get_work_mode() != LOGIC) {
        r(QJsonValue(), "vcd export needs logic data; use csv");
        return;
    }
    if (!path.endsWith("." + format)) {
        r(QJsonValue(), "path must end with ." + format);
        return;
    }
    if (!_session->have_view_data()) {
        r(QJsonValue(), "no captured data to export");
        return;
    }
    StoreSession *store = new StoreSession(_session);
    store->SetFileName(path, format);
    start_store(store, path, true, r);
}

void ControlApi::measure_logic(const QJsonObject &p, const Reply &r)
{
    data::LogicSnapshot *snapshot = NULL;
    QString err = check_logic_data(snapshot);
    if (!err.isEmpty()) {
        r(QJsonValue(), err);
        return;
    }
    int channel;
    if (!req_int(p, "channel", channel) || !snapshot->has_data(channel)) {
        r(QJsonValue(), "parameter 'channel' must be a captured logic channel index");
        return;
    }
    uint64_t count = snapshot->get_ring_sample_count();
    uint64_t start = 0, end = count - 1;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end)) {
        r(QJsonValue(), "start and end must be non-negative integers");
        return;
    }
    end = std::min(end, count - 1);
    if (start > end) {
        r(QJsonValue(), "start must not be greater than end");
        return;
    }

    bool initial, truncated;
    std::vector<uint64_t> edges;
    walk_edges(snapshot, channel, start, end, 10000000, initial, edges, truncated);

    const double rate = snapshot->samplerate();
    const double ns = rate > 0 ? 1e9 / rate : 0;
    const size_t n = edges.size();

    // After edge j the line is high when initial ^ ((j + 1) & 1); the pulse
    // between edges j and j+1 has that level.
    auto high_after = [&](size_t j) { return (bool)(initial ^ ((j + 1) & 1)); };

    uint64_t rising = 0, falling = 0;
    int64_t first_rise = -1, last_rise = -1;
    uint64_t min_high = UINT64_MAX, max_high = 0, min_low = UINT64_MAX, max_low = 0;

    for (size_t j = 0; j < n; j++) {
        if (high_after(j)) {
            rising++;
            if (first_rise < 0)
                first_rise = (int64_t)j;
            last_rise = (int64_t)j;
        }
        else {
            falling++;
        }
        if (j + 1 < n) {
            uint64_t w = edges[j + 1] - edges[j];
            if (high_after(j)) {
                min_high = std::min(min_high, w);
                max_high = std::max(max_high, w);
            }
            else {
                min_low = std::min(min_low, w);
                max_low = std::max(max_low, w);
            }
        }
    }

    // Whole periods between the first and the last rising edge.
    uint64_t periods = rising > 0 ? rising - 1 : 0;
    uint64_t period_total = 0, high_total = 0;
    if (periods > 0) {
        period_total = edges[last_rise] - edges[first_rise];
        for (int64_t j = first_rise; j < last_rise; j++) {
            if (high_after(j))
                high_total += edges[j + 1] - edges[j];
        }
    }

    QJsonObject o;
    o["channel"] = channel;
    o["start"] = num(start);
    o["end"] = num(end);
    o["initial_level"] = initial ? 1 : 0;
    o["rising_edges"] = num(rising);
    o["falling_edges"] = num(falling);
    if (periods > 0) {
        double period = (double)period_total / periods;
        o["period_ns"] = period * ns;
        o["frequency_hz"] = rate / period;
        if (period_total > 0)
            o["duty_percent"] = 100.0 * high_total / period_total;
    }
    if (max_high > 0) {
        o["min_high_ns"] = min_high * ns;
        o["max_high_ns"] = max_high * ns;
    }
    if (max_low > 0) {
        o["min_low_ns"] = min_low * ns;
        o["max_low_ns"] = max_low * ns;
    }
    if (truncated)
        o["truncated"] = true;
    r(o, QString());
}

void ControlApi::measure_dso(const QJsonObject &p, const Reply &r)
{
    if (_session->get_device()->get_work_mode() != DSO) {
        r(QJsonValue(), "measure_dso needs oscilloscope mode");
        return;
    }
    int channel;
    if (!req_int(p, "channel", channel)) {
        r(QJsonValue(), "parameter 'channel' is required");
        return;
    }
    view::DsoSignal *sig = find_dso_signal(channel);
    if (sig == NULL || !sig->enabled()) {
        r(QJsonValue(), QString("dso channel %1 is not enabled").arg(channel));
        return;
    }
    QJsonObject m;
    for (const auto &d : DsoMeasures)
        m[d.name] = sig->get_measure((DSO_MEASURE_TYPE)d.type);
    QJsonObject o;
    o["channel"] = channel;
    o["measurements"] = m;
    o["note"] = "values are formatted as shown in DSView; '--' means not available";
    r(o, QString());
}

void ControlApi::show_region(const QJsonObject &p, const Reply &r)
{
    uint64_t start = 0, end = 0;
    if (!opt_u64(p, "start", start) || !opt_u64(p, "end", end) || end <= start) {
        r(QJsonValue(), "start and end are sample indexes with end > start");
        return;
    }
    if (!_session->have_view_data()) {
        r(QJsonValue(), "no captured data");
        return;
    }
    _window->_view->show_region(start, end, false);
    QJsonObject o;
    o["shown"] = true;
    r(o, QString());
}

void ControlApi::set_cursors(const QJsonObject &p, const Reply &r)
{
    if (!p["samples"].isArray()) {
        r(QJsonValue(), "parameter 'samples' must be an array of sample indexes (empty clears)");
        return;
    }
    QJsonArray samples = p["samples"].toArray();
    if (samples.size() > 16) {
        r(QJsonValue(), "at most 16 cursors");
        return;
    }
    _window->_view->clear_cursors();
    for (const QJsonValue &v : samples) {
        if (!v.isDouble() || v.toDouble() < 0) {
            r(QJsonValue(), "cursor positions must be non-negative sample indexes");
            return;
        }
        _window->_view->add_cursor((uint64_t)v.toDouble());
    }
    _window->_view->show_cursors(!samples.isEmpty());
    QJsonObject o;
    o["cursors"] = samples.size();
    r(o, QString());
}

void ControlApi::version(const QJsonObject &, const Reply &r)
{
    QJsonObject o;
    o["application"] = "DSView";
    o["version"] = QCoreApplication::applicationVersion();
    o["api"] = 1;

    // Which implementation each migrated subsystem runs (DSVIEW_IMPL*).
    QJsonObject impl;
    for (const char *sub : {"FORMATS", "STORE", "DECODE", "DRV_DEMO", "DRV_FILE", "DRV_DSLOGIC", "DRV_DSCOPE", "ACQ"}) {
        bool rust = false;
#ifdef DSVIEW_RUST_CORE
        rust = dsv_use_rust(sub) != 0;
#endif
        impl[QString(sub).toLower()] = rust ? "rust" : "c";
    }
    o["implementations"] = impl;
    r(o, QString());
}

} // namespace mcp
} // namespace pv
