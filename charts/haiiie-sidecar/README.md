# haiiie sidecar chart

This chart creates one `YesnoCluster` and a Service for the haiiie peer in the
operator-managed Pods. It does not create a second Deployment or mount yesnod's
data PVC in haiiie. The operator creates the shared `/run/yesno/plugin.sock`
volume, gives both containers uid 10001, and restarts the peer with its instance
on promotion. The chart's default Service selects the **current leader** by the
operator's role label; set `service.followers.enabled=true` only when potentially
stale reads are acceptable.

## Prerequisites and image

Install yesno's `yesno-operator` CRD and controller first. Provide a local
filesystem StorageClass suitable for yesnodb's mmap-backed data, and build the
yesnod image as described by the operator. Neither upstream nor this chart
publishes an image. The haiiie image must contain `/usr/local/bin/haiiied` as
its entrypoint and `/usr/local/bin/haiiie` for the query-aware readiness probe.
The supplied build uses the exact public yesno commit in `YESNO_REVISION`:

```console
docker build -f dist/Dockerfile -t haiiie:local .
helm lint charts/haiiie-sidecar
helm template search charts/haiiie-sidecar
```

Load or push both images so the cluster can pull them. Replace `yesnod:local`
and `haiiie:local` with those image names. The operator's `storageClassName`
and the chart's cert-manager names are cluster-specific.

## Evaluation install

The default values are an **unauthenticated evaluation profile** for a trusted
namespace. yesnod's Flight and control listeners use plaintext, and haiiie's
gRPC listener has no authentication or TLS. The sidecar reaches Flight and
control on Pod loopback to create a 256-bit binary index in namespace 1 and to
checkpoint. There are two yesnod instances, one leader and one asynchronous
follower. Query traffic goes to the leader Service unless the follower Service
is explicitly enabled.

```console
helm install search charts/haiiie-sidecar --namespace search --create-namespace \
  --set yesno.image=registry.example/yesnod:tag \
  --set plugin.image=registry.example/haiiie:tag \
  --set yesno.storage.storageClassName=local-storage
kubectl get yesnocluster search -n search
kubectl get service search-haiiie-rw -n search
```

The Service is `ClusterIP`; expose it only through a boundary that supplies
client authentication and transport protection. There is no chart-managed
Ingress. A follower can lag an acknowledged leader write, and yesno replication
is asynchronous, so failover alone is not a zero-data-loss guarantee.

`haiiied` retries only missing, refused or temporarily unavailable peer opens
while yesnod starts or reboots. Invalid index metadata, a wrong residual model
and malformed channel frames fail rather than looping. Readiness executes
`haiiie describe`, which performs a snapshot-backed read; checking only the
socket would report ready before yesnod has opened its database. Liveness
checks the local gRPC port so a temporarily unavailable snapshot does not
restart a live process.

## Secure, read-only profile

`values-secure.yaml` selects the operator's cert-manager mTLS topology and
turns off the peer's Flight and control connections. It opens an **existing**
haiiie index; it cannot initialize an empty PVC. Set the Issuer and CA Secret
names to resources in the same namespace, then render with:

```console
helm template search charts/haiiie-sidecar -f charts/haiiie-sidecar/values-secure.yaml
```

The operator does not mount client credentials or arbitrary Secret volumes into
`spec.plugin`, and haiiie's peer Flight/control workers do not use client mTLS.
The template therefore rejects enabling those workers in secure mode. A fully
secure writable deployment needs those two interfaces connected; this chart
does not pretend that bearer-token files or a loopback URL satisfy mTLS.
Even in this profile, haiiie's **client-facing** gRPC port has no TLS or auth;
keep its Service behind a trusted boundary. A model-bound residual index can be
served if the matching model file is baked into the plugin image and
`plugin.index.residualModelPath` names it.

The chart supports one haiiie namespace per `YesnoCluster`. Its Service
selectors use the operator's stable `app.kubernetes.io/instance` and
`yesnodb.io/role` labels, so leader promotion updates routing without a Helm
upgrade. Keep the release/cluster name to a simple DNS label of at most 48
characters; longer names are rewritten by the operator and would break a
selector copied verbatim from the chart.
