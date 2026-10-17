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

#ifndef DSVIEW_PV_MCP_CONTROLSERVER_H
#define DSVIEW_PV_MCP_CONTROLSERVER_H

#include <QObject>
#include <QByteArray>
#include <QJsonObject>
#include <QString>

class QLocalServer;
class QLocalSocket;

namespace pv {

class MainWindow;

namespace mcp {

class ControlApi;

// Local control endpoint for the dsview-mcp bridge: newline-delimited
// JSON-RPC 2.0 over a Unix socket that only the current user can open.
class ControlServer : public QObject
{
    Q_OBJECT

public:
    static const int MaxLineBytes = 1024 * 1024;

    explicit ControlServer(MainWindow *window, QObject *parent = nullptr);
    ~ControlServer();

    bool start();

    inline QString socket_path() const {
        return _path;
    }

    // $DSVIEW_MCP_SOCKET, else $XDG_RUNTIME_DIR/dsview/mcp.sock, else ~/.local/state/dsview/mcp.sock.
    static QString default_socket_path();

private slots:
    void on_new_connection();
    void on_ready_read();

private:
    void handle_line(QLocalSocket *socket, const QByteArray &line);
    static void send(QLocalSocket *socket, const QJsonObject &msg);
    static void send_error(QLocalSocket *socket, const QJsonValue &id, int code, const QString &message);

    QLocalServer *_server;
    ControlApi *_api;
    QString _path;
};

} // namespace mcp
} // namespace pv

#endif // DSVIEW_PV_MCP_CONTROLSERVER_H
