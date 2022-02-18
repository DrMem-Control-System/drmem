// Copyright (c) 2020-2022, Richard M Neswold, Jr.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are
// met:
//
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its
//    contributors may be used to endorse or promote products derived
//    from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use drmem_api::{driver, Result};
use drmem_types::DrMemError;
use std::collections::{hash_map, HashMap};
use tokio::{
    select,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tracing::{info_span, warn};
use tracing_futures::Instrument;

/// Stores information associated with devices. The key is the full
/// name of the device.
///
/// The value is a 2-tuple where the first element is the send handle
/// of a broadcast channel. The second element is an optional handle
/// to transmit settings to the driver.
struct DeviceMap(
    HashMap<String, (driver::TxDeviceValue, Option<driver::TxDeviceSetting>)>,
);

impl DeviceMap {
    fn new() -> Self {
        DeviceMap(HashMap::new())
    }

    fn insert_ro_device(
        &mut self, device_name: String,
    ) -> Option<driver::TxDeviceValue> {
        if let hash_map::Entry::Vacant(e) = self.0.entry(device_name) {
            let (tx, _) = broadcast::channel(20);
            let _ = e.insert((tx.clone(), None));

            Some(tx)
        } else {
            None
        }
    }

    fn insert_rw_device(
        &mut self, device_name: String,
    ) -> Option<(driver::TxDeviceValue, driver::RxDeviceSetting)> {
        if let hash_map::Entry::Vacant(e) = self.0.entry(device_name) {
            let (tx_val, _) = broadcast::channel(20);
            let (tx_setting, rx_setting) = mpsc::channel(20);
            let _ = e.insert((tx_val.clone(), Some(tx_setting)));

            Some((tx_val, rx_setting))
        } else {
            None
        }
    }
}

/// Holds the state of the core task in the framework.
///
/// The core task starts-up the necessary drivers and maintains a
/// table of active devices. Drivers and client communicate with the
/// core task through channels.
struct State {
    devices: DeviceMap,
}

impl State {
    /// Creates an initialized state for the core task.
    fn create() -> Self {
        State {
            devices: DeviceMap::new(),
        }
    }

    fn send_reply<T>(
        dev_name: &str, rpy_chan: oneshot::Sender<Result<T>>, val: Option<T>,
    ) {
        let result = val
            .ok_or_else(|| DrMemError::DeviceDefined(String::from(dev_name)));

        if rpy_chan.send(result).is_err() {
            warn!("driver exited before a reply could be sent")
        }
    }

    async fn handle_driver_request(&mut self, req: driver::Request) {
        match req {
            driver::Request::AddReadonlyDevice {
                ref dev_name,
                rpy_chan,
            } => {
                let result = self.devices.insert_ro_device(dev_name.into());

                State::send_reply(dev_name, rpy_chan, result)
            }

            driver::Request::AddReadWriteDevice {
                ref dev_name,
                rpy_chan,
            } => {
                let result = self.devices.insert_rw_device(dev_name.into());

                State::send_reply(dev_name, rpy_chan, result)
            }
        }
    }

    /// Captures the State and runs as a async task using it as its
    /// mutable state. Normally it is run as a background task using
    /// `task::spawn`.
    async fn run(
        mut self, mut rx_drv_req: mpsc::Receiver<driver::Request>,
    ) -> Result<()> {
        loop {
            select! {
		Some(req) = rx_drv_req.recv() => {
                    self.handle_driver_request(req).await
		}
		else => {
                    warn!("no active drivers left ... exiting");
                    return Ok(())
		}
            }
        }
    }
}

pub fn start() -> (mpsc::Sender<driver::Request>, JoinHandle<Result<()>>) {
    // Create a channel that drivers can use to make requests to the
    // framework. This task will hang onto the Receiver end and each
    // driver will get a .clone() of the transmit handle.

    let (tx_drv_req, rx_drv_req) = mpsc::channel(10);

    (
        tx_drv_req,
        tokio::spawn(
            State::create()
                .run(rx_drv_req)
                .instrument(info_span!("core")),
        ),
    )
}
