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

#ifndef DSVIEW_PV_MCP_CONTROLAPI_H
#define DSVIEW_PV_MCP_CONTROLAPI_H

#include <QObject>
#include <QJsonObject>
#include <QJsonArray>
#include <QJsonValue>
#include <QString>
#include <QTimer>
#include <QElapsedTimer>
#include <functional>
#include <map>
#include <vector>
#include "../interface/icallbacks.h"

namespace pv {

class MainWindow;
class SigSession;
class StoreSession;

namespace data {
class LogicSnapshot;
}

namespace view {
class DsoSignal;
class DecodeTrace;
}

namespace mcp {

// Completes one request: an empty error means success.
typedef std::function<void(const QJsonValue &result, const QString &error)> Reply;

// Executes control requests against the running GUI. Everything here runs on
// the GUI thread; long operations (capture, decode, save) reply later.
class ControlApi : public QObject, public IMessageListener
{
public:
    explicit ControlApi(MainWindow *window, QObject *parent = nullptr);
    ~ControlApi();

    void dispatch(const QString &method, const QJsonObject &params, const Reply &reply);

    // IMessageListener
    void OnMessage(int msg) override;

private:
    typedef void (ControlApi::*Handler)(const QJsonObject &p, const Reply &r);

    struct CaptureWaiter {
        Reply reply;
        qint64 deadline;
        qint64 start_by;
        bool started;
    };

    struct DecodeWaiter {
        Reply reply;
        qint64 deadline;
        std::vector<int> indexes;
    };

    struct StoreJob {
        Reply reply;
        StoreSession *store;
        QString path;
        bool is_export;
    };

    // device + capture
    void list_devices(const QJsonObject &p, const Reply &r);
    void select_device(const QJsonObject &p, const Reply &r);
    void get_config(const QJsonObject &p, const Reply &r);
    void set_mode(const QJsonObject &p, const Reply &r);
    void set_sampling(const QJsonObject &p, const Reply &r);
    void set_channels(const QJsonObject &p, const Reply &r);
    void set_trigger(const QJsonObject &p, const Reply &r);
    void set_threshold(const QJsonObject &p, const Reply &r);
    void set_dso(const QJsonObject &p, const Reply &r);
    void start_capture(const QJsonObject &p, const Reply &r);
    void stop_capture(const QJsonObject &p, const Reply &r);
    void capture_status(const QJsonObject &p, const Reply &r);

    // readout
    void capture_info(const QJsonObject &p, const Reply &r);
    void get_edges(const QJsonObject &p, const Reply &r);
    void read_logic(const QJsonObject &p, const Reply &r);
    void read_waveform(const QJsonObject &p, const Reply &r);

    // decoders
    void list_decoders(const QJsonObject &p, const Reply &r);
    void describe_decoder(const QJsonObject &p, const Reply &r);
    void add_decoder(const QJsonObject &p, const Reply &r);
    void list_active_decoders(const QJsonObject &p, const Reply &r);
    void remove_decoder(const QJsonObject &p, const Reply &r);
    void run_decoders(const QJsonObject &p, const Reply &r);
    void get_annotations(const QJsonObject &p, const Reply &r);

    // files, measurements, view
    void open_file(const QJsonObject &p, const Reply &r);
    void save_session(const QJsonObject &p, const Reply &r);
    void export_data(const QJsonObject &p, const Reply &r);
    void measure_logic(const QJsonObject &p, const Reply &r);
    void measure_dso(const QJsonObject &p, const Reply &r);
    void show_region(const QJsonObject &p, const Reply &r);
    void set_cursors(const QJsonObject &p, const Reply &r);
    void version(const QJsonObject &p, const Reply &r);

    // helpers
    QJsonObject config_json();
    QJsonObject capture_info_json();
    QJsonObject decoder_state_json(int index);
    QString check_idle();
    QString check_logic_data(data::LogicSnapshot *&snapshot);
    view::DsoSignal* find_dso_signal(int channel);
    void walk_edges(data::LogicSnapshot *snapshot, int channel, uint64_t start, uint64_t end,
                    uint64_t limit, bool &initial, std::vector<uint64_t> &edges, bool &truncated);
    void wait_capture(const Reply &r, double timeout_s);
    void start_store(StoreSession *store, const QString &path, bool is_export, const Reply &r);
    void ensure_polling();
    void poll();

    MainWindow *_window;
    SigSession *_session;
    std::map<QString, Handler> _handlers;
    std::vector<QString> _mutating;
    std::vector<CaptureWaiter> _capture_waiters;
    std::vector<DecodeWaiter> _decode_waiters;
    std::vector<StoreJob> _store_jobs;
    QTimer _poll_timer;
    QElapsedTimer _clock;
};

} // namespace mcp
} // namespace pv

#endif // DSVIEW_PV_MCP_CONTROLAPI_H
