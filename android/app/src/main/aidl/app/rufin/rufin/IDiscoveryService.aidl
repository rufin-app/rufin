package app.rufin.rufin;

import app.rufin.rufin.IDiscoverySession;

interface IDiscoveryService {
    IDiscoverySession open(long timeoutSeconds);
}
