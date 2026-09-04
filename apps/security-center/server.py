from claw_os_sdk.mcp import App

from main import auth, events, mac, ports, ssh, sudo, summary


app = App.from_manifest()


@app.tool("security-center.summary")
def security_summary():
    return summary()


@app.tool("security-center.auth")
def authentication_events():
    return auth()


@app.tool("security-center.ssh")
def ssh_security():
    return ssh()


@app.tool("security-center.sudo")
def sudo_policy():
    return sudo()


@app.tool("security-center.mac")
def mandatory_access_control():
    return mac()


@app.tool("security-center.ports")
def listening_ports():
    return ports()


@app.tool("security-center.events")
def security_events():
    return events()


if __name__ == "__main__":
    app.serve()
