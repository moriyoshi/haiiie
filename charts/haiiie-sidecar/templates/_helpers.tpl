{{- define "haiiie-sidecar.clusterName" -}}
{{- $name := default .Release.Name .Values.cluster.name -}}
{{- if or (gt (len $name) 48) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$" $name)) -}}
{{- fail "cluster.name (or the release name) must be a DNS label of at most 48 characters" -}}
{{- end -}}
{{- $name -}}
{{- end -}}
